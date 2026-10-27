// The practice and technique repositories answer a deleted or missing
// parent alike: a delete of one writes nothing, and a save over one fails
// before importing any file.
import 'dart:io';

import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/asset_store.dart';
import 'package:vocara/data/repositories/song_practice_repository_drift.dart';
import 'package:vocara/data/repositories/technique_repository_drift.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/sync_hashes.dart';
import 'package:vocara/data/sync/sync_writer.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/practice_summary.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/domain/models/technique_media.dart';
import 'package:vocara/utils/id.dart';
import 'package:vocara/utils/result.dart';

void main() {
  late AppDatabase db;
  late Directory tmp;
  late SongPracticeRepositoryDrift songs;
  late TechniqueRepositoryDrift techs;

  setUp(() async {
    db = AppDatabase(NativeDatabase.memory());
    tmp = await Directory.systemTemp.createTemp('parent_guards');
    final writer = SyncWriter(db, HlcClock(node: 'NODE'.padRight(26, '0')));
    final assets = AssetStore(db: db, root: tmp, writer: writer);
    songs = SongPracticeRepositoryDrift(db: db, writer: writer, assets: assets);
    techs = TechniqueRepositoryDrift(db: db, writer: writer, assets: assets);
  });
  tearDown(() async {
    await db.close();
    if (await tmp.exists()) await tmp.delete(recursive: true);
  });

  Future<String> file(String name, List<int> bytes) async {
    final f = File(p.join(tmp.path, 'staging', name));
    await f.parent.create(recursive: true);
    await f.writeAsBytes(bytes);
    return f.path;
  }

  Future<List<String>> ops() async => [
    for (final o in await db.select(db.oplog).get()) '${o.entityTable}:${o.op}',
  ];

  Future<Loaded<SongPractice>> seedPractice() async {
    final created = (await songs.createPractice('S') as Ok<SongPractice>).value;
    final take = AudioTrack(
      id: newId(),
      kind: AudioTrackKind.vocal,
      displayName: 'Take',
      createdAt: DateTime(2026),
      versionId: created.versions.single.id,
      clips: [
        AudioClip(
          id: newId(),
          audioFilePath: await file('a.wav', [1, 2, 3]),
          durationMs: 1000,
        ),
      ],
    );
    final loaded =
        (await songs.loadPractice(created.id) as Ok<Loaded<SongPractice>>)
            .value;
    return (await songs.savePracticeWithBase(
              loaded.aggregate.copyWith(vocalTracks: [take]),
              baseHashes: loaded.baseHashes,
            )
            as Ok<Loaded<SongPractice>>)
        .value;
  }

  Future<Loaded<Technique>> seedTechnique() async {
    final t = Technique(
      id: newId(),
      name: 'Twang',
      media: [
        TechniqueMedia(
          id: newId(),
          path: await file('m.wav', [4, 5, 6]),
          kind: TechniqueMediaKind.audio,
          addedAt: DateTime(2026),
        ),
      ],
      createdAt: DateTime(2026),
      updatedAt: DateTime(2026),
    );
    return (await techs.saveTechniqueWithBase(t, baseHashes: const {})
            as Ok<Loaded<Technique>>)
        .value;
  }

  test('deleting a deleted or unknown parent writes nothing', () async {
    // Tombstoned again, it journaled a fresh delete (for a row that never
    // existed, too).
    final p0 = await seedPractice();
    final t0 = await seedTechnique();
    expect(await songs.deletePractice(p0.aggregate.id), isA<Ok<void>>());
    expect(await techs.deleteTechnique(t0.aggregate.id), isA<Ok<void>>());
    final before = await ops();
    expect(await songs.deletePractice(p0.aggregate.id), isA<Ok<void>>());
    expect(await techs.deleteTechnique(t0.aggregate.id), isA<Ok<void>>());
    expect(await songs.deletePractice(newId()), isA<Ok<void>>());
    expect(await techs.deleteTechnique(newId()), isA<Ok<void>>());
    expect(await ops(), before);
  });

  test('a save over a deleted parent imports nothing', () async {
    // Refused only inside the row transaction, its new files had already
    // been imported and journaled for a save that then failed.
    final p0 = await seedPractice();
    final t0 = await seedTechnique();
    await songs.deletePractice(p0.aggregate.id);
    await techs.deleteTechnique(t0.aggregate.id);
    final before = await ops();
    final take = p0.aggregate.vocalTracks.single;
    final practiceSave = await songs.savePracticeWithBase(
      p0.aggregate.copyWith(
        vocalTracks: [
          take.copyWith(
            clips: [
              ...take.clips,
              AudioClip(
                id: newId(),
                audioFilePath: await file('new.wav', [7, 8, 9]),
                durationMs: 500,
                startMs: 2000,
              ),
            ],
          ),
        ],
      ),
      baseHashes: p0.baseHashes,
    );
    final techniqueSave = await techs.saveTechniqueWithBase(
      t0.aggregate.copyWith(
        media: [
          ...t0.aggregate.media,
          TechniqueMedia(
            id: newId(),
            path: await file('new.m4a', [10, 11, 12]),
            kind: TechniqueMediaKind.audio,
            addedAt: DateTime(2026),
          ),
        ],
      ),
      baseHashes: t0.baseHashes,
    );
    expect(practiceSave, isA<Error<Loaded<SongPractice>>>());
    expect(techniqueSave, isA<Error<Loaded<Technique>>>());
    expect(await ops(), before);
  });

  test(
    'a stored time past what a DateTime holds still lists and opens',
    () async {
      // Every list of the library threw on one such row, and showed nothing.
      final p0 = await seedPractice();
      final t0 = await seedTechnique();
      await db.customStatement(
        'UPDATE practices SET updated_at = ${1 << 62}, created_at = ${-(1 << 62)}',
      );
      await db.customStatement('UPDATE techniques SET updated_at = ${1 << 62}');

      final listed = await songs.listPracticeSummaries();
      expect(
        (listed as Ok<List<PracticeSummary>>).value.single.id,
        p0.aggregate.id,
      );
      expect(
        await songs.loadPractice(p0.aggregate.id),
        isA<Ok<Loaded<SongPractice>>>(),
      );
      final techniques = await techs.listTechniques();
      expect(
        (techniques as Ok<List<Technique>>).value.single.id,
        t0.aggregate.id,
      );
    },
  );

  test('a stored level or pan past the faders loads within them', () async {
    // A peer's level went to the engine verbatim: 1e30 a gain.
    final p0 = await seedPractice();
    await db.customStatement('UPDATE tracks SET volume = 1e30, pan = -7.5');
    await db.customStatement('UPDATE practices SET master_volume = 1e30');
    final loaded =
        (await songs.loadPractice(p0.aggregate.id) as Ok<Loaded<SongPractice>>)
            .value
            .aggregate;
    expect(loaded.masterVolume, 1.0);
    for (final t in loaded.vocalTracks) {
      expect(t.volume, 1.0);
      expect(t.pan, -1.0);
    }
  });
}
