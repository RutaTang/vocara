import 'package:drift/drift.dart' hide isNull, isNotNull;
import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/sync_applier.dart';

String _id(String s) => s.padRight(26, '0').substring(0, 26);

void main() {
  late AppDatabase db;
  late SyncApplier applier;
  final remoteNode = _id('REMOTE');

  setUp(() {
    db = AppDatabase(NativeDatabase.memory());
    applier = SyncApplier(db, HlcClock(node: _id('LOCAL'), now: () => 1000));
  });
  tearDown(() => db.close());

  Hlc hlc(int wall, [int ctr = 0]) =>
      Hlc(wallMs: wall, counter: ctr, node: remoteNode);

  RemoteRecord practice(String id, String title, Hlc h, {int? deletedAt}) =>
      RemoteRecord(table: 'practices', columns: {
        'id': _id(id),
        'title': title,
        'timeline_duration_ms': 0,
        'master_volume': 1.0,
        'created_at': h.wallMs,
        'updated_at': h.wallMs,
        'deleted_at': deletedAt,
        'hlc_wall': h.wallMs,
        'hlc_ctr': h.counter,
        'hlc_node': h.node,
        'hlc_packed': h.packed,
        'schema_v': 1,
      });

  Future<Map<String, Object?>?> row(String id) async {
    final r = await db
        .customSelect(
          'SELECT title, deleted_at FROM practices WHERE id = ?',
          variables: [Variable.withString(_id(id))],
        )
        .getSingleOrNull();
    return r?.data;
  }

  test('a new record is inserted', () async {
    final res = await applier.apply([practice('P', 'Hello', hlc(900))]);
    expect(res.applied, 1);
    expect((await row('P'))!['title'], 'Hello');
  });

  test('a higher-HLC update wins; a lower-HLC update is skipped', () async {
    await applier.apply([practice('P', 'v1', hlc(900))]);

    final win = await applier.apply([practice('P', 'v2', hlc(950))]);
    expect(win.applied, 1);
    expect((await row('P'))!['title'], 'v2');

    final lose = await applier.apply([practice('P', 'stale', hlc(920))]);
    expect(lose.skipped, 1);
    expect(lose.applied, 0);
    expect((await row('P'))!['title'], 'v2'); // unchanged
  });

  test('a tombstone arrives as a higher-HLC update', () async {
    await applier.apply([practice('P', 'v1', hlc(900))]);
    final res =
        await applier.apply([practice('P', 'v1', hlc(970), deletedAt: 970)]);
    expect(res.applied, 1);
    expect((await row('P'))!['deleted_at'], isNotNull);
  });

  test('an implausibly-future record is quarantined, not applied', () async {
    // now=1000, default maxDrift=5min -> wall 5min+1 ahead is rejected.
    final res = await applier.apply([
      practice('FUT', 'from the future', hlc(1000 + 5 * 60 * 1000 + 1)),
    ]);
    expect(res.quarantined, 1);
    expect(res.applied, 0);
    expect(await row('FUT'), isNull);
  });

  test('a malformed record is quarantined without discarding the batch',
      () async {
    final good = practice('GOOD', 'kept', hlc(900));
    // A record carrying a column that does not exist locally (e.g. from a newer
    // schema_v) fails at INSERT — a NON-drift error. It must roll back only itself
    // (its SAVEPOINT), not the whole batch, so the already-applied good record stays.
    final bad = RemoteRecord(table: 'practices', columns: {
      ...practice('BAD', 'dropped', hlc(910)).columns,
      'column_from_the_future': 1,
    });
    final res = await applier.apply([good, bad]);
    expect(res.applied, 1);
    expect(res.failed, 1);
    expect((await row('GOOD'))!['title'], 'kept'); // survived the later failure
    expect(await row('BAD'), isNull); // rolled back to its savepoint
  });

  test('applying remote changes writes NO oplog (no ping-pong)', () async {
    await applier.apply([
      practice('A', 'a', hlc(900)),
      practice('B', 'b', hlc(910)),
    ]);
    await applier.apply([practice('A', 'a2', hlc(950))]);
    expect(await db.select(db.oplog).get(), isEmpty);
  });

  test('the local clock advances past an applied remote stamp', () async {
    await applier.apply([practice('P', 'x', hlc(2000))]);
    // A subsequent local tick must be strictly after the remote stamp we saw.
    final next = applier.clock.tick();
    expect(next.wallMs >= 2000, isTrue);
  });
}
