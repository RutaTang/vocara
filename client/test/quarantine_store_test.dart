import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/sync/quarantine_store.dart';

String _id(String s) => s.padRight(26, '0').substring(0, 26);

void main() {
  late AppDatabase db;
  late QuarantineStore store;

  setUp(() {
    db = AppDatabase(NativeDatabase.memory());
    store = QuarantineStore(db);
  });
  tearDown(() => db.close());

  Future<void> put(
    String table,
    String id, {
    String hlcPacked = '0000000001000-000000-N',
    String reason = 'hlc-drift',
    int at = 1000,
  }) => store.put(
    table: table,
    id: _id(id),
    hlcPacked: hlcPacked,
    schemaV: 1,
    payload: '{"stub":true}',
    reason: reason,
    quarantinedAt: at,
  );

  test('put / list / delete round-trip', () async {
    await put('practices', 'P1', at: 100);
    await put('tracks', 'T1', at: 50);

    final rows = await store.list();
    expect(rows, hasLength(2));
    // Oldest quarantine first.
    expect(rows[0].tbl, 'tracks');
    expect(rows[1].tbl, 'practices');
    expect(rows[1].id, _id('P1'));
    expect(rows[1].schemaV, 1);
    expect(rows[1].payload, '{"stub":true}');

    await store.delete('practices', _id('P1'));
    expect((await store.list()).single.tbl, 'tracks');

    await store.delete('practices', _id('P1')); // missing row: no-op
  });

  test('re-quarantining a row keeps only the LATEST state (upsert on the '
      '(tbl, id) primary key)', () async {
    await put('practices', 'P1', reason: 'hlc-drift: old', at: 100);
    await put(
      'practices',
      'P1',
      hlcPacked: '0000000002000-000000-N',
      reason: 'apply-failed: new',
      at: 200,
    );

    final rows = await store.list();
    expect(rows, hasLength(1));
    expect(rows.single.reason, 'apply-failed: new');
    expect(rows.single.remoteHlcPacked, '0000000002000-000000-N');
    expect(rows.single.quarantinedAt, 200);
  });

  test('a strictly OLDER park is a no-op: the store is LWW by hlcPacked, '
      'not last-caller-wins (parks are not ordered by arrival)', () async {
    await put(
      'practices',
      'P1',
      hlcPacked: '0000000002000-000000-N',
      reason: 'hlc-drift: newest state, parked at the door',
      at: 100,
    );
    // The end-of-batch FK sweep can legally park an OLDER db-row state of
    // the same row later in the same page; the newest parked state must
    // survive it untouched.
    await put(
      'practices',
      'P1',
      hlcPacked: '0000000001000-000000-N',
      reason: 'fk-orphan: stale sweep park',
      at: 200,
    );

    final rows = await store.list();
    expect(rows, hasLength(1));
    expect(rows.single.remoteHlcPacked, '0000000002000-000000-N');
    expect(rows.single.reason, 'hlc-drift: newest state, parked at the door');
    expect(rows.single.payload, '{"stub":true}');
    expect(rows.single.quarantinedAt, 100, reason: 'entirely untouched');
  });

  test('an EQUAL-HLC re-park still refreshes reason/payload (the fk-orphan '
      'replay cycle re-parks the identical state every pass)', () async {
    await put('practices', 'P1', reason: 'fk-orphan: first pass', at: 100);
    await put('practices', 'P1', reason: 'fk-orphan: second pass', at: 200);

    final rows = await store.list();
    expect(rows, hasLength(1));
    expect(rows.single.reason, 'fk-orphan: second pass');
    expect(rows.single.quarantinedAt, 200);
  });

  test('the same id under two different tables parks two rows (composite '
      'key)', () async {
    await put('practices', 'X');
    await put('tracks', 'X');
    expect(await store.list(), hasLength(2));
  });

  test('writes JOIN a caller transaction (rolled back together)', () async {
    await expectLater(
      db.transaction(() async {
        await put('practices', 'P1');
        throw StateError('outer apply failed');
      }),
      throwsStateError,
    );
    expect(await store.list(), isEmpty);
  });

  test('CLOCK-POISON GUARD: sync_quarantine must never carry a column named '
      'hlc_packed', () async {
    // Persistence._seedClock seeds the boot HLC from MAX(hlc_packed) of every
    // table carrying that column, and syncedTableRegistry keys off the same
    // SyncCols names. Quarantined stamps are by definition suspect (often
    // far-future), so the stamp column is remote_hlc_packed by design — this
    // test is the tripwire against an accidental rename back.
    final cols = await db
        .customSelect('PRAGMA table_info(sync_quarantine)')
        .get();
    final names = cols.map((c) => c.read<String>('name')).toSet();
    expect(names, isNot(contains('hlc_packed')));
    expect(names, contains('remote_hlc_packed'));
  });
}
