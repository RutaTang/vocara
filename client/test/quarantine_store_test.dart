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
    String payload = '{"stub":true}',
    int at = 1000,
  }) => store.put(
    table: table,
    id: _id(id),
    hlcPacked: hlcPacked,
    schemaV: 1,
    payload: payload,
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

  group('replay-class retention (a permanently-unreplayable park must never '
      'displace or block a replayable one)', () {
    test('a NEWER unreplayable park does not displace a stored fk-orphan '
        'park — the parked snapshot is the only copy of a swept row', () async {
      await put(
        'practices',
        'P1',
        hlcPacked: '0000000001000-000000-N',
        reason: 'fk-orphan: parent assets/A missing',
        payload: '{"honest":true}',
        at: 100,
      );
      // A non-conforming writer's newer state parked at the value gate. Its
      // gate re-rejects the identical payload every replay cycle, so it can
      // never supersede anything — letting it hold the slot destroys the
      // only surviving copy of the swept row.
      await put(
        'practices',
        'P1',
        hlcPacked: '0000000002000-000000-N',
        reason: 'invalid-value: lyrics_json is JSON but not the shape',
        payload: '{"garbage":true}',
        at: 200,
      );

      final rows = await store.list();
      expect(rows, hasLength(1));
      expect(rows.single.remoteHlcPacked, '0000000001000-000000-N');
      expect(rows.single.reason, startsWith('fk-orphan'));
      expect(rows.single.payload, '{"honest":true}');
    });

    test(
      'an fk-orphan park replaces stored unreplayable garbage even when '
      'strictly OLDER — the sweep hard-deletes the row right after',
      () async {
        // Same-page ordering: the door parks the newer malformed state first,
        // then the end-of-batch FK sweep parks the current db row. A filtered
        // sweep park followed by the sweep DELETE is permanent row loss.
        await put(
          'practices',
          'P1',
          hlcPacked: '0000000002000-000000-N',
          reason: 'invalid-value: lyrics_json is JSON but not the shape',
          payload: '{"garbage":true}',
          at: 100,
        );
        await put(
          'practices',
          'P1',
          hlcPacked: '0000000001000-000000-N',
          reason: 'fk-orphan: parent assets/A missing',
          payload: '{"honest":true}',
          at: 200,
        );

        final rows = await store.list();
        expect(rows, hasLength(1));
        expect(rows.single.remoteHlcPacked, '0000000001000-000000-N');
        expect(rows.single.reason, startsWith('fk-orphan'));
        expect(rows.single.payload, '{"honest":true}');
      },
    );

    test('between two unreplayable parks the newest still wins (nothing '
        'replayable is at stake, keep the freshest evidence)', () async {
      await put(
        'practices',
        'P1',
        hlcPacked: '0000000001000-000000-N',
        reason: 'invalid-id: separator in id',
        at: 100,
      );
      await put(
        'practices',
        'P1',
        hlcPacked: '0000000002000-000000-N',
        reason: 'invalid-value: mistyped column',
        at: 200,
      );
      expect(
        (await store.list()).single.remoteHlcPacked,
        '0000000002000-000000-N',
      );

      // And the older direction stays a no-op, like within any class.
      await put(
        'practices',
        'P1',
        hlcPacked: '0000000000500-000000-N',
        reason: 'invalid-value: older garbage',
        at: 300,
      );
      expect(
        (await store.list()).single.remoteHlcPacked,
        '0000000002000-000000-N',
      );
    });

    test('an EQUAL stamp replaces ACROSS classes too: the same record state '
        'whose replay verdict changed relabels the entry', () async {
      // E.g. a record first parked for HLC drift whose replay (clock
      // recovered) now fails the value gate: same stamp, new verdict.
      await put(
        'practices',
        'P1',
        hlcPacked: '0000000001000-000000-N',
        reason: 'hlc-drift: 6 minutes ahead',
        at: 100,
      );
      await put(
        'practices',
        'P1',
        hlcPacked: '0000000001000-000000-N',
        reason: 'invalid-value: mistyped column',
        at: 200,
      );
      final rows = await store.list();
      expect(rows.single.reason, startsWith('invalid-value'));
      expect(rows.single.quarantinedAt, 200);
    });
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

  test('deleteIfUnchanged settles only the exact replayed state, never a '
      'newer one parked meanwhile (replay TOCTOU)', () async {
    // The replay lists Q@100, applies it, then deletes. If Q@200 was parked
    // during the apply (a concurrent pull page), the unconditional delete
    // dropped Q@200 unreplayed. The conditional delete leaves it for the
    // next cycle.
    const old = '0000000000100-000000-N';
    await put('practices', 'P', hlcPacked: old);
    await store.deleteIfUnchanged(
      'practices',
      _id('P'),
      hlcPacked: old,
      payload: '{"stub":true}',
    );
    expect(await store.list(), isEmpty, reason: 'the exact state settles');

    await put('practices', 'P', hlcPacked: old);
    // The LWW put replaces the entry with a NEWER state mid-replay…
    await put('practices', 'P', hlcPacked: '0000000000200-000000-N');
    // …and the settle of the OLD stamp must not touch it.
    await store.deleteIfUnchanged(
      'practices',
      _id('P'),
      hlcPacked: old,
      payload: '{"stub":true}',
    );
    final left = await store.list();
    expect(left, hasLength(1));
    expect(left.single.remoteHlcPacked, '0000000000200-000000-N');
  });

  test('an EQUAL stamp with a refreshed payload also survives the settle '
      '(the stamp alone is not proof of identity)', () async {
    // put deliberately lets an equal stamp replace the stored entry with a
    // fresh reason/payload — the FK-orphan re-park cycle depends on it — so a
    // stamp match cannot prove the entry is still the one that was replayed.
    const stamp = '0000000000100-000000-N';
    await put('practices', 'P', hlcPacked: stamp, payload: '{"v":1}');
    await put('practices', 'P', hlcPacked: stamp, payload: '{"v":2}');

    await store.deleteIfUnchanged(
      'practices',
      _id('P'),
      hlcPacked: stamp,
      payload: '{"v":1}',
    );

    final left = await store.list();
    expect(left, hasLength(1), reason: 'the re-parked payload must survive');
    expect(left.single.payload, '{"v":2}');
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
