import 'dart:io';

import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/asset_store.dart';
import 'package:vocara/data/repositories/song_practice_repository_drift.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/sync_writer.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/utils/id.dart';
import 'package:vocara/utils/result.dart';

void main() {
  late AppDatabase db;
  late Directory tmp;
  late AssetStore assets;
  late SongPracticeRepositoryDrift repo;

  setUp(() async {
    db = AppDatabase(NativeDatabase.memory());
    tmp = await Directory.systemTemp.createTemp('vocara_gc');
    final writer = SyncWriter(db, HlcClock(node: 'NODE'.padRight(26, '0')));
    assets = AssetStore(db: db, root: tmp, writer: writer);
    repo = SongPracticeRepositoryDrift(db: db, writer: writer, assets: assets);
  });
  tearDown(() async {
    await db.close();
    if (await tmp.exists()) await tmp.delete(recursive: true);
  });

  Future<String> makeSrc(String name, List<int> bytes) async {
    final f = File(p.join(tmp.path, name));
    await f.writeAsBytes(bytes);
    return f.path;
  }

  test('sweep tombstones + deletes a wholly-unreferenced asset', () async {
    final id = await assets.importFile(await makeSrc('lone.wav', [1, 2, 3]));
    final path = (await assets.resolve(id))!;
    expect(await File(path).exists(), isTrue);

    expect(await assets.sweepUnreferenced(), 1);
    expect(await assets.resolve(id), isNull); // tombstoned
    expect(await File(path).exists(), isFalse); // file reclaimed
  });

  test('sweepOrphanPoolFiles reclaims a file left after a tombstone (crash)',
      () async {
    final id = await assets.importFile(await makeSrc('crash.wav', [5, 6, 7]));
    final path = (await assets.resolve(id))!;
    expect(await File(path).exists(), isTrue);
    // Simulate a crash between the tombstone commit and the file unlink in
    // sweepUnreferenced: the row is tombstoned but the pooled file is still on disk.
    // sweepUnreferenced never revisits it (it iterates only live rows), so the
    // orphan sweep — trusting only LIVE hashes — must reclaim it.
    await db.customStatement('UPDATE assets SET deleted_at = 1 WHERE id = ?', [id]);
    expect(await File(path).exists(), isTrue);
    expect(await assets.sweepOrphanPoolFiles(), greaterThanOrEqualTo(1));
    expect(await File(path).exists(), isFalse); // reclaimed
  });

  test('sweep deletes every pooled file for a hash (same bytes, two extensions)',
      () async {
    final id = await assets.importFile(await makeSrc('dup.wav', [7, 7, 7]));
    final wavPath = (await assets.resolve(id))!;
    // Simulate identical bytes pooled under a different extension by another device
    // (same hash, `<hash>.wav` AND `<hash>.mp3`).
    final mp3Path = wavPath.replaceAll(RegExp(r'\.wav$'), '.mp3');
    await File(wavPath).copy(mp3Path);
    expect(await File(mp3Path).exists(), isTrue);
    // The asset is unreferenced -> sweep tombstones it and deletes ALL files for the
    // hash, not just the current row's `.wav`.
    expect(await assets.sweepUnreferenced(), 1);
    expect(await File(wavPath).exists(), isFalse);
    expect(await File(mp3Path).exists(), isFalse);
  });

  test('a referenced asset survives sweep, then is collectable once unused',
      () async {
    final base =
        (await repo.createPractice('S') as Ok<SongPractice>).value;
    final clipPath = await makeSrc('take.wav', [9, 9, 9]);
    final vocal = AudioTrack(
      id: newId(),
      kind: AudioTrackKind.vocal,
      displayName: 'Take',
      createdAt: DateTime(2026),
      versionId: base.versions.single.id,
      clips: [AudioClip(id: newId(), audioFilePath: clipPath, durationMs: 1000)],
    );
    final withClip = SongPractice(
      id: base.id,
      title: base.title,
      createdAt: base.createdAt,
      updatedAt: base.updatedAt,
      vocalTracks: [vocal],
      versions: base.versions,
      activeVersionId: base.versions.single.id,
    );
    (await repo.savePractice(withClip) as Ok<SongPractice>).value;

    // One referenced asset + one standalone unreferenced asset.
    await assets.importFile(await makeSrc('orphan.wav', [4, 4]));
    expect(await db.select(db.assets).get(), hasLength(2));

    // Sweep removes only the orphan; the clip's asset survives.
    expect(await assets.sweepUnreferenced(), 1);
    final liveAssets = await (db.select(db.assets)
          ..where((a) => a.deletedAt.isNull()))
        .get();
    expect(liveAssets, hasLength(1));

    // Drop the clip -> its asset is now unreferenced -> a later sweep reclaims it.
    (await repo.savePractice(SongPractice(
      id: base.id,
      title: base.title,
      createdAt: base.createdAt,
      updatedAt: base.updatedAt,
      versions: base.versions,
      activeVersionId: base.versions.single.id,
    )) as Ok<SongPractice>)
        .value;
    expect(await assets.sweepUnreferenced(), 1);
    final remaining = await (db.select(db.assets)
          ..where((a) => a.deletedAt.isNull()))
        .get();
    expect(remaining, isEmpty);
  });
}
