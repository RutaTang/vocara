import 'package:drift/drift.dart' hide isNull, isNotNull;
import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/reconcile.dart';
import 'package:vocara/data/sync/sync_writer.dart';

/// A tiny desired-item for the test, mapped onto the `interpretations` table.
class _Interp {
  _Interp(this.id, this.name, this.order);
  final String id;
  final String name;
  final int order;
}

String _id(String s) => s.padRight(26, '0').substring(0, 26);

void main() {
  late AppDatabase db;
  late SyncWriter writer;
  final practiceId = _id('P1');

  setUp(() async {
    db = AppDatabase(NativeDatabase.memory());
    writer = SyncWriter(db, HlcClock(node: _id('NODE')));
    // A parent practice the interpretations hang off (FK).
    final s = writer.stamp();
    await db.into(db.practices).insert(
          PracticesCompanion.insert(
            id: practiceId,
            createdAt: s.at,
            updatedAt: s.at,
            hlcWall: s.hlc.wallMs,
            hlcCtr: s.hlc.counter,
            hlcNode: s.hlc.node,
            hlcPacked: s.hlc.packed,
          ),
        );
  });
  tearDown(() => db.close());

  String hashOf(_Interp i) => SyncWriter.contentHashOf([i.name, i.order]);

  Future<Map<String, String>> currentHashes() async {
    final rows = await (db.select(db.interpretations)
          ..where((t) => t.deletedAt.isNull()))
        .get();
    return {for (final r in rows) r.id: SyncWriter.contentHashOf([r.name, r.orderIndex])};
  }

  Future<ReconcileResult> sync(List<_Interp> desired) async {
    return db.transaction(() async {
      final current = await currentHashes();
      return reconcileCollection<_Interp>(
        writer: writer,
        table: 'interpretations',
        currentHashes: current,
        desired: desired,
        idOf: (i) => i.id,
        hashOf: hashOf,
        insert: (i, s) => db.into(db.interpretations).insert(
              InterpretationsCompanion.insert(
                id: i.id,
                practiceId: practiceId,
                name: Value(i.name),
                orderIndex: Value(i.order),
                createdAt: s.at,
                updatedAt: s.at,
                hlcWall: s.hlc.wallMs,
                hlcCtr: s.hlc.counter,
                hlcNode: s.hlc.node,
                hlcPacked: s.hlc.packed,
              ),
            ),
        update: (i, s) =>
            (db.update(db.interpretations)..where((t) => t.id.equals(i.id)))
                .write(InterpretationsCompanion(
              name: Value(i.name),
              orderIndex: Value(i.order),
              updatedAt: Value(s.at),
              hlcWall: Value(s.hlc.wallMs),
              hlcCtr: Value(s.hlc.counter),
              hlcNode: Value(s.hlc.node),
              hlcPacked: Value(s.hlc.packed),
            )),
        tombstone: (id, s) =>
            (db.update(db.interpretations)..where((t) => t.id.equals(id)))
                .write(InterpretationsCompanion(
              deletedAt: Value(s.at),
              updatedAt: Value(s.at),
              hlcWall: Value(s.hlc.wallMs),
              hlcCtr: Value(s.hlc.counter),
              hlcNode: Value(s.hlc.node),
              hlcPacked: Value(s.hlc.packed),
            )),
      );
    });
  }

  Future<int> oplogCount() async => (await db.select(db.oplog).get()).length;

  test('first save inserts everything', () async {
    final r = await sync([_Interp(_id('A'), 'Tender', 0), _Interp(_id('B'), 'Powerful', 1)]);
    expect(r.inserts, 2);
    expect(r.updates, 0);
    expect(r.tombstones, 0);
    expect(await oplogCount(), 2);
  });

  test('a no-op save changes nothing (zero new oplog rows)', () async {
    final items = [_Interp(_id('A'), 'Tender', 0), _Interp(_id('B'), 'Powerful', 1)];
    await sync(items);
    final before = await oplogCount();
    final r = await sync(items); // identical
    expect(r.total, 0);
    expect(await oplogCount(), before);
  });

  test('only the changed row is updated; siblings stay untouched', () async {
    final a = _Interp(_id('A'), 'Tender', 0);
    final b = _Interp(_id('B'), 'Powerful', 1);
    await sync([a, b]);
    final before = await oplogCount();
    final r = await sync([_Interp(_id('A'), 'Tender', 0), _Interp(_id('B'), 'Belted', 1)]);
    expect(r.updates, 1);
    expect(r.inserts, 0);
    expect(r.tombstones, 0);
    expect(await oplogCount(), before + 1);
    final rowB = await (db.select(db.interpretations)..where((t) => t.id.equals(_id('B')))).getSingle();
    expect(rowB.name, 'Belted');
  });

  test('a removed item is tombstoned, not hard-deleted', () async {
    await sync([_Interp(_id('A'), 'Tender', 0), _Interp(_id('B'), 'Powerful', 1)]);
    final r = await sync([_Interp(_id('A'), 'Tender', 0)]); // B dropped
    expect(r.tombstones, 1);
    final rowB = await (db.select(db.interpretations)..where((t) => t.id.equals(_id('B')))).getSingle();
    expect(rowB.deletedAt, isNotNull); // still there, just tombstoned
    // It no longer appears as live, so a re-add reuses a fresh insert path.
    final live = await currentHashes();
    expect(live.keys, [_id('A')]);
  });

  test('insert + update + tombstone in one reconcile', () async {
    await sync([_Interp(_id('A'), 'Tender', 0), _Interp(_id('B'), 'Powerful', 1)]);
    final r = await sync([
      _Interp(_id('A'), 'Soft', 0), // changed
      _Interp(_id('C'), 'Raspy', 2), // new
      // B removed
    ]);
    expect(r.inserts, 1);
    expect(r.updates, 1);
    expect(r.tombstones, 1);
  });
}
