import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/sync_writer.dart';

String _node() => 'NODE'.padRight(26, '0');

void main() {
  late AppDatabase db;
  late SyncWriter writer;

  setUp(() {
    db = AppDatabase(NativeDatabase.memory());
    var now = 1000;
    writer = SyncWriter(db, HlcClock(node: _node(), now: () => now));
  });
  tearDown(() => db.close());

  test('stamp ticks the HLC monotonically', () {
    final a = writer.stamp();
    final b = writer.stamp();
    expect(b.hlc > a.hlc, isTrue);
  });

  test('journal writes one oplog row (committed with the txn)', () async {
    final s = writer.stamp();
    await db.transaction(() async {
      await writer.journal(
        table: 'practices',
        id: 'P'.padRight(26, '0'),
        op: 'insert',
        hlc: s.hlc,
        payloadHash: 'abc',
      );
    });
    final rows = await db.select(db.oplog).get();
    expect(rows, hasLength(1));
    expect(rows.single.entityTable, 'practices');
    expect(rows.single.op, 'insert');
    expect(rows.single.hlcPacked, s.hlc.packed);
    expect(rows.single.synced, isFalse);
  });

  test('contentHashOf is canonical (key order does not change it)', () {
    final h1 = SyncWriter.contentHashOf([
      'title',
      {'a': 1, 'b': 2},
      42,
    ]);
    final h2 = SyncWriter.contentHashOf([
      'title',
      {'b': 2, 'a': 1}, // reordered
      42,
    ]);
    expect(h1, h2);
    final h3 = SyncWriter.contentHashOf([
      'title',
      {'a': 1, 'b': 3},
      42,
    ]);
    expect(h3, isNot(h1));
  });

  test('contentHashOf rejects non-finite doubles with a clear ArgumentError '
      '(jsonEncode would throw an unactionable Error instead)', () {
    expect(() => SyncWriter.contentHashOf([double.nan]), throwsArgumentError);
    expect(
      () => SyncWriter.contentHashOf(['x', double.infinity]),
      throwsArgumentError,
    );
    // Nested values are guarded too (the canonicalizer recurses).
    expect(
      () => SyncWriter.contentHashOf([
        {'pan': double.negativeInfinity},
      ]),
      throwsArgumentError,
    );
    expect(
      () => SyncWriter.contentHashOf([
        [1.0, double.nan],
      ]),
      throwsArgumentError,
    );
    // Ordinary finite doubles keep hashing deterministically.
    expect(
      SyncWriter.contentHashOf([1.0, -0.5]),
      SyncWriter.contentHashOf([1.0, -0.5]),
    );
  });
}
