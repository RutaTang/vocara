import 'dart:io';

import 'package:drift/drift.dart' hide isNull, isNotNull;
import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/asset_store.dart';
import 'package:vocara/data/repositories/song_practice_repository_drift.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/integrity_checker.dart';
import 'package:vocara/data/sync/sync_writer.dart';
import 'package:vocara/domain/models/audio_fx.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/fx_bus.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/utils/id.dart';
import 'package:vocara/utils/result.dart';

void main() {
  late AppDatabase db;
  late Directory tmp;
  late SongPracticeRepositoryDrift repo;
  late IntegrityChecker checker;

  setUp(() async {
    db = AppDatabase(NativeDatabase.memory());
    tmp = await Directory.systemTemp.createTemp('vocara_integrity');
    final writer = SyncWriter(db, HlcClock(node: 'NODE'.padRight(26, '0')));
    final assets = AssetStore(db: db, root: tmp, writer: writer);
    repo = SongPracticeRepositoryDrift(db: db, writer: writer, assets: assets);
    checker = IntegrityChecker(db, writer);
  });
  tearDown(() async {
    await db.close();
    if (await tmp.exists()) await tmp.delete(recursive: true);
  });

  /// Seeds a practice with one vocal take (one clip) and returns it.
  Future<SongPractice> seed() async {
    final base = (await repo.createPractice('S') as Ok<SongPractice>).value;
    final f = File(p.join(tmp.path, 'take.wav'));
    await f.writeAsBytes([1, 2, 3]);
    final vocal = AudioTrack(
      id: newId(),
      kind: AudioTrackKind.vocal,
      displayName: 'Take',
      createdAt: DateTime(2026),
      versionId: base.versions.single.id,
      clips: [AudioClip(id: newId(), audioFilePath: f.path, durationMs: 1000)],
    );
    return (await repo.savePractice(
              SongPractice(
                id: base.id,
                title: base.title,
                createdAt: base.createdAt,
                updatedAt: base.updatedAt,
                vocalTracks: [vocal],
                versions: base.versions,
                activeVersionId: base.versions.single.id,
              ),
            )
            as Ok<SongPractice>)
        .value;
  }

  Future<void> rawTombstone(String table, String id) => db.customUpdate(
    'UPDATE $table SET deleted_at = 999 WHERE id = ?',
    variables: [Variable.withString(id)],
  );

  Future<String> firstId(String table) async {
    final r = await db
        .customSelect('SELECT id FROM $table WHERE deleted_at IS NULL LIMIT 1')
        .getSingle();
    return r.read<String>('id');
  }

  test('a well-formed practice has no violations', () async {
    await seed();
    expect(await checker.check(), isEmpty);
  });

  test(
    'a live clip under a tombstoned track is detected + cascade-repaired',
    () async {
      await seed();
      await rawTombstone('tracks', await firstId('tracks'));

      final found = await checker.check();
      expect(found.map((v) => v.invariant), contains('clip-track'));

      expect(await checker.repair(), isEmpty);
      // The orphaned clip is now tombstoned too.
      final liveClips = await db
          .customSelect(
            'SELECT COUNT(*) AS n FROM clips WHERE deleted_at IS NULL',
          )
          .getSingle();
      expect(liveClips.read<int>('n'), 0);
    },
  );

  test('a dangling active_interpretation_id is nulled', () async {
    final pr = await seed();
    await rawTombstone('interpretations', pr.versions.single.id);

    final found = await checker.check();
    expect(found.map((v) => v.invariant), contains('active-interp'));

    await checker.repair();
    final row = await db
        .customSelect(
          'SELECT active_interpretation_id AS a FROM practices WHERE id = ?',
          variables: [Variable.withString(pr.id)],
        )
        .getSingle();
    expect(row.data['a'], isNull);
    expect(await checker.check(), isEmpty);
  });

  test('a clip whose asset was tombstoned is repaired', () async {
    await seed();
    await rawTombstone('assets', await firstId('assets'));

    expect(
      (await checker.check()).map((v) => v.invariant),
      contains('clip-asset'),
    );
    expect(await checker.repair(), isEmpty);
  });

  test('a bus routing cycle synthesised by a merge is detected and broken '
      'at its oldest edge', () async {
    // Neither edit is illegal on its own — every device refuses to create a
    // cycle locally — but A→B applied offline on one device and B→A on
    // another merge into a ring. The raw UPDATEs stand in for that merge.
    final base = await seed();
    final a = FxBus(id: newId(), name: 'A', fx: TrackFx.flat());
    final b = FxBus(id: newId(), name: 'B', fx: TrackFx.flat());
    await repo.savePractice(base.copyWith(fxBuses: [a, b]));

    // A is the older edit (lower HLC), so it is the one that must lose its
    // route: the newer B→A survives, matching the LWW rule everywhere else.
    const node = 'PEER';
    String stamp(int wall) =>
        Hlc(wallMs: wall, counter: 0, node: node.padRight(26, '0')).packed;
    await db.customUpdate(
      'UPDATE fx_buses SET output_bus_id = ?, hlc_packed = ? WHERE id = ?',
      variables: [
        Variable.withString(b.id),
        Variable.withString(stamp(1000)),
        Variable.withString(a.id),
      ],
    );
    await db.customUpdate(
      'UPDATE fx_buses SET output_bus_id = ?, hlc_packed = ? WHERE id = ?',
      variables: [
        Variable.withString(a.id),
        Variable.withString(stamp(2000)),
        Variable.withString(b.id),
      ],
    );

    final found = await checker.check();
    expect(found.map((v) => v.invariant), everyElement('bus-cycle'));
    expect(found.map((v) => v.id).toSet(), {a.id, b.id});

    expect(await checker.repair(), isEmpty);
    final rows = await db
        .customSelect('SELECT id, output_bus_id FROM fx_buses')
        .get();
    final routes = {
      for (final r in rows)
        r.read<String>('id'): r.read<String?>('output_bus_id'),
    };
    expect(routes[a.id], isNull, reason: 'the older edge is dropped');
    expect(routes[b.id], a.id, reason: 'the newer edge survives');
  });

  test('a self-route is a cycle of one and is repaired', () async {
    final base = await seed();
    final a = FxBus(id: newId(), name: 'A', fx: TrackFx.flat());
    await repo.savePractice(base.copyWith(fxBuses: [a]));
    await db.customUpdate(
      'UPDATE fx_buses SET output_bus_id = id WHERE id = ?',
      variables: [Variable.withString(a.id)],
    );

    expect(
      (await checker.check()).map((v) => v.invariant),
      contains('bus-cycle'),
    );
    expect(await checker.repair(), isEmpty);
  });

  test('repair composes into an enclosing transaction: a rollback reverts '
      'both the row mutations and their oplog entries', () async {
    // Persistence.repairLibrary (like the sync engine's end-of-pull repair)
    // wraps repair() in ONE db.transaction so each repair's row UPDATE and
    // its journal entry can never commit separately. This proves the repair
    // internals genuinely compose into a caller transaction: everything a
    // full repair pass wrote is undone atomically by a rollback.
    await seed();
    await rawTombstone('tracks', await firstId('tracks'));
    expect(await checker.check(), isNotEmpty);
    final opsBefore = (await db.select(db.oplog).get()).length;

    await expectLater(
      db.transaction(() async {
        await checker.repair();
        throw StateError('injected post-repair crash');
      }),
      throwsStateError,
    );

    // The violations are all still present and not one journal entry leaked.
    expect(await checker.check(), isNotEmpty);
    expect((await db.select(db.oplog).get()).length, opsBefore);

    // The same repair inside a COMMITTED transaction resolves everything and
    // journals it.
    await db.transaction(() => checker.repair());
    expect(await checker.check(), isEmpty);
    expect((await db.select(db.oplog).get()).length, greaterThan(opsBefore));
  });
}
