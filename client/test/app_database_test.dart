import 'dart:io';

import 'package:drift/drift.dart';
import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/db/app_database.dart';

/// A 26-char ULID-shaped stub id for tests.
String _id(String seed) => seed.padRight(26, '0').substring(0, 26);

PracticesCompanion _practice(String id, {int now = 1000}) =>
    PracticesCompanion.insert(
      id: _id(id),
      createdAt: now,
      updatedAt: now,
      hlcWall: now,
      hlcCtr: 0,
      hlcNode: _id('NODE'),
      hlcPacked: '0000000001000-000000-${_id('NODE')}',
    );

TracksCompanion _track(
  String id,
  String practiceId,
  String kind, {
  int? deletedAt,
  int now = 1000,
}) => TracksCompanion.insert(
  id: _id(id),
  practiceId: _id(practiceId),
  kind: kind,
  createdAt: now,
  updatedAt: now,
  hlcWall: now,
  hlcCtr: 0,
  hlcNode: _id('NODE'),
  hlcPacked: '0000000001000-000000-${_id('NODE')}',
  deletedAt: Value(deletedAt),
);

void main() {
  late AppDatabase db;

  setUp(() => db = AppDatabase(NativeDatabase.memory()));
  tearDown(() => db.close());

  test('schema creates and a practice round-trips', () async {
    await db.into(db.practices).insert(_practice('P1'));
    final rows = await db.select(db.practices).get();
    expect(rows, hasLength(1));
    expect(rows.single.id, _id('P1'));
    expect(rows.single.masterVolume, 1.0); // default applied
  });

  test('CHECK rejects an invalid track kind', () async {
    await db.into(db.practices).insert(_practice('P1'));
    expect(
      () => db.into(db.tracks).insert(_track('T1', 'P1', 'bogus')),
      throwsA(isA<SqliteException>()),
    );
  });

  test('foreign key is enforced (clip needs a real track + asset)', () async {
    expect(
      () => db
          .into(db.clips)
          .insert(
            ClipsCompanion.insert(
              id: _id('C1'),
              trackId: _id('NOPE'),
              assetId: _id('NOPE'),
              createdAt: 1000,
              updatedAt: 1000,
              hlcWall: 1000,
              hlcCtr: 0,
              hlcNode: _id('NODE'),
              hlcPacked: 'x',
            ),
          ),
      throwsA(isA<SqliteException>()),
    );
  });

  test(
    'partial unique index allows ONE live accompaniment per practice',
    () async {
      await db.into(db.practices).insert(_practice('P1'));
      await db.into(db.tracks).insert(_track('T1', 'P1', 'accompaniment'));
      // A second LIVE accompaniment for the same practice is rejected.
      expect(
        () => db.into(db.tracks).insert(_track('T2', 'P1', 'accompaniment')),
        throwsA(isA<SqliteException>()),
      );
      // Tombstoning the live one frees the slot (a tombstoned row is excluded
      // from the partial index), so a fresh live accompaniment is then allowed.
      await (db.update(db.tracks)..where((t) => t.id.equals(_id('T1')))).write(
        const TracksCompanion(deletedAt: Value(2000)),
      );
      await db.into(db.tracks).insert(_track('T4', 'P1', 'accompaniment'));
      final live =
          await (db.select(db.tracks)..where(
                (t) => t.kind.equals('accompaniment') & t.deletedAt.isNull(),
              ))
              .get();
      expect(live, hasLength(1));
      expect(live.single.id, _id('T4'));
    },
  );

  test(
    'multiple vocal tracks per practice are allowed (slot rule is scoped)',
    () async {
      await db.into(db.practices).insert(_practice('P1'));
      await db.into(db.tracks).insert(_track('V1', 'P1', 'vocal'));
      await db.into(db.tracks).insert(_track('V2', 'P1', 'vocal'));
      final vocals = await (db.select(
        db.tracks,
      )..where((t) => t.kind.equals('vocal'))).get();
      expect(vocals, hasLength(2));
    },
  );

  test('derived indexes are re-applied on every open, so one added in a '
      'newer build reaches an existing library without a migration', () async {
    Future<Set<String>> indexNames(AppDatabase d) async {
      final rows = await d
          .customSelect(
            "SELECT name FROM sqlite_master WHERE type = 'index' "
            "AND name LIKE 'ix\\_%' ESCAPE '\\'",
          )
          .get();
      return {for (final r in rows) r.read<String>('name')};
    }

    final dir = await Directory.systemTemp.createTemp('vocara_idx');
    addTearDown(() => dir.delete(recursive: true));
    final file = File(p.join(dir.path, 'library.db'));

    // First open (onCreate): all derived indexes exist, the oplog HLC-seed
    // index among them.
    var onDisk = AppDatabase(NativeDatabase(file));
    await onDisk.customSelect('SELECT 1').get(); // force the lazy open
    expect(await indexNames(onDisk), contains('ix_oplog_hlc'));
    // Simulate a library created by an OLDER build that never had the index.
    await onDisk.customStatement('DROP INDEX ix_oplog_hlc');
    await onDisk.close();

    // Re-open: beforeOpen re-runs the derived indexes idempotently, so the
    // "new" index appears with no schema-version bump and no migration.
    onDisk = AppDatabase(NativeDatabase(file));
    addTearDown(onDisk.close);
    expect(await indexNames(onDisk), contains('ix_oplog_hlc'));
  });
}
