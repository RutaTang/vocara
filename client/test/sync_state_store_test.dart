import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/sync/sync_state_store.dart';

void main() {
  late AppDatabase db;
  late SyncStateStore store;

  setUp(() {
    db = AppDatabase(NativeDatabase.memory());
    store = SyncStateStore(db);
  });
  tearDown(() => db.close());

  test('a missing key reads null', () async {
    expect(await store.get('nope'), isNull);
  });

  test('set / get / overwrite / remove round-trip', () async {
    await store.set('cursor', 'a');
    expect(await store.get('cursor'), 'a');

    await store.set('cursor', 'b'); // upsert, not a PK violation
    expect(await store.get('cursor'), 'b');

    await store.remove('cursor');
    expect(await store.get('cursor'), isNull);

    await store.remove('cursor'); // removing a missing key is a no-op
  });

  test('typed change-token accessors', () async {
    expect(await store.changeToken(), isNull);
    await store.setChangeToken('tok-1');
    expect(await store.changeToken(), 'tok-1');
    await store.setChangeToken('tok-2');
    expect(await store.changeToken(), 'tok-2');
    await store.clearChangeToken();
    expect(await store.changeToken(), isNull);
  });

  test('writes JOIN a caller transaction: a rolled-back apply also rolls back '
      'the token it would have acknowledged', () async {
    await store.setChangeToken('before');
    await expectLater(
      db.transaction(() async {
        // The apply-loop shape: mutate rows, then persist the token — if the
        // commit fails, acknowledging the page would lose those records.
        await store.setChangeToken('after');
        throw StateError('apply failed');
      }),
      throwsStateError,
    );
    expect(await store.changeToken(), 'before'); // token rolled back too
  });

  test('writes inside a COMMITTED caller transaction persist', () async {
    await db.transaction(() async {
      await store.setChangeToken('committed');
      await store.set('cursor', 'page-9');
    });
    expect(await store.changeToken(), 'committed');
    expect(await store.get('cursor'), 'page-9');
  });
}
