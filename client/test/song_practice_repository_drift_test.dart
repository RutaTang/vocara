import 'dart:io';

import 'package:drift/drift.dart' show Table, TableInfo;
import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/asset_store.dart';
import 'package:vocara/data/repositories/song_practice_repository_drift.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/sync_hashes.dart';
import 'package:vocara/data/sync/sync_writer.dart';
import 'package:vocara/domain/models/audio_fx.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/fx_bus.dart';
import 'package:vocara/domain/models/lyric_line.dart';
import 'package:vocara/domain/models/practice_note.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/domain/models/technique_marker.dart';
import 'package:vocara/utils/id.dart';
import 'package:vocara/utils/result.dart';

void main() {
  late AppDatabase db;
  late Directory tmp;
  late SongPracticeRepositoryDrift repo;

  setUp(() async {
    db = AppDatabase(NativeDatabase.memory());
    tmp = await Directory.systemTemp.createTemp('vocara_practice_repo');
    final writer = SyncWriter(db, HlcClock(node: 'NODE'.padRight(26, '0')));
    repo = SongPracticeRepositoryDrift(
      db: db,
      writer: writer,
      assets: AssetStore(db: db, root: tmp, writer: writer),
    );
  });
  tearDown(() async {
    await db.close();
    if (await tmp.exists()) await tmp.delete(recursive: true);
  });

  Future<AudioClip> clip(
    String name,
    List<int> bytes, {
    List<double> peaks = const [],
  }) async {
    final f = File(p.join(tmp.path, name));
    await f.writeAsBytes(bytes);
    return AudioClip(
      id: newId(),
      audioFilePath: f.path,
      durationMs: 5000,
      startMs: 0,
      waveformPeaks: peaks,
    );
  }

  SongPractice ok(Result<SongPractice> r) => (r as Ok<SongPractice>).value;

  test('createPractice seeds exactly one interpretation', () async {
    final pr = ok(await repo.createPractice('My Song'));
    expect(pr.title, 'My Song');
    expect(pr.versions, hasLength(1));
    expect(pr.activeVersionId, pr.versions.single.id);
  });

  test('full graph round-trips through save -> load', () async {
    final base = ok(await repo.createPractice('Hai Kuo'));
    final versionId = base.versions.single.id;

    final accomp = AudioTrack(
      id: newId(),
      kind: AudioTrackKind.accompaniment,
      displayName: 'Backing',
      createdAt: DateTime(2026),
      clips: [
        await clip('backing.wav', [1, 2, 3]),
      ],
    );
    final vocal = AudioTrack(
      id: newId(),
      kind: AudioTrackKind.vocal,
      displayName: 'Take 1',
      createdAt: DateTime(2026),
      versionId: versionId,
      volume: 0.7,
      muted: true,
      clips: [
        await clip('take.wav', [9, 8, 7], peaks: [0.5, 0.25, 0.75]),
      ],
      notes: [
        PracticeNote(
          id: newId(),
          positionMs: 1200,
          content: 'breathy onset',
          createdAt: DateTime(2026),
          updatedAt: DateTime(2026),
        ),
      ],
      techniqueMarkers: [
        TechniqueMarker(
          id: newId(),
          positionMs: 2000,
          techniqueId: 'tech-1',
          techniqueName: 'Twang',
          createdAt: DateTime(2026),
          updatedAt: DateTime(2026),
        ),
      ],
    );
    final bus = FxBus(id: newId(), name: 'Reverb Bus', fx: TrackFx.flat());

    final enriched = SongPractice(
      id: base.id,
      title: base.title,
      createdAt: base.createdAt,
      updatedAt: base.updatedAt,
      accompaniment: accomp,
      vocalTracks: [vocal],
      versions: base.versions,
      activeVersionId: versionId,
      lyrics: const [LyricLine(text: 'hai kuo tian kong', timeMs: 0)],
      timelineDurationMs: 30000,
      masterVolume: 0.8,
      fxBuses: [bus],
    );
    ok(await repo.savePractice(enriched));

    final loaded = ok(await repo.getPractice(base.id));
    expect(loaded.masterVolume, 0.8);
    expect(loaded.timelineDurationMs, 30000);
    expect(loaded.activeVersionId, versionId);
    expect(loaded.lyrics.single.text, 'hai kuo tian kong');
    expect(loaded.fxBuses.single.name, 'Reverb Bus');

    // Accompaniment + its clip resolve to a readable pooled file.
    expect(loaded.accompaniment, isNotNull);
    expect(
      await File(
        loaded.accompaniment!.clips.single.audioFilePath,
      ).readAsBytes(),
      [1, 2, 3],
    );

    // Vocal track + children.
    expect(loaded.vocalTracks, hasLength(1));
    final v = loaded.vocalTracks.single;
    expect(v.versionId, versionId);
    expect(v.volume, 0.7);
    expect(v.muted, isTrue);
    expect(v.notes.single.content, 'breathy onset');
    expect(v.techniqueMarkers.single.techniqueName, 'Twang');
    // Waveform peaks survive (cached by content hash).
    expect(v.clips.single.waveformPeaks, [0.5, 0.25, 0.75]);
  });

  test(
    'editing the title bumps updatedAt; an unchanged save is a no-op',
    () async {
      final v1 = ok(await repo.createPractice('A'));
      final beforeOps = (await db.select(db.oplog).get()).length;

      final v1b = ok(await repo.savePractice(v1)); // identical
      expect(v1b.updatedAt, v1.updatedAt);
      expect((await db.select(db.oplog).get()).length, beforeOps);

      final v2 = ok(
        await repo.savePractice(
          SongPractice(
            id: v1.id,
            title: 'A renamed',
            createdAt: v1.createdAt,
            updatedAt: v1.updatedAt,
            versions: v1.versions,
            activeVersionId: v1.activeVersionId,
          ),
        ),
      );
      expect(v2.title, 'A renamed');
      expect(v2.updatedAt.isAfter(v1.updatedAt), isTrue);
    },
  );

  test(
    'removing a vocal track cascade-tombstones its clips/notes/markers',
    () async {
      final base = ok(await repo.createPractice('S'));
      final vocal = AudioTrack(
        id: newId(),
        kind: AudioTrackKind.vocal,
        displayName: 'Take',
        createdAt: DateTime(2026),
        versionId: base.versions.single.id,
        clips: [
          await clip('t.wav', [1]),
        ],
        notes: [
          PracticeNote(
            id: newId(),
            positionMs: 0,
            content: 'x',
            createdAt: DateTime(2026),
            updatedAt: DateTime(2026),
          ),
        ],
      );
      ok(
        await repo.savePractice(
          SongPractice(
            id: base.id,
            title: base.title,
            createdAt: base.createdAt,
            updatedAt: base.updatedAt,
            vocalTracks: [vocal],
            versions: base.versions,
            activeVersionId: base.activeVersionId,
          ),
        ),
      );
      expect(await _liveCount(db, db.clips), 1);
      expect(await _liveCount(db, db.practiceNotes), 1);

      // Save without the vocal track.
      ok(
        await repo.savePractice(
          SongPractice(
            id: base.id,
            title: base.title,
            createdAt: base.createdAt,
            updatedAt: base.updatedAt,
            versions: base.versions,
            activeVersionId: base.activeVersionId,
          ),
        ),
      );
      final loaded = ok(await repo.getPractice(base.id));
      expect(loaded.vocalTracks, isEmpty);
      expect(await _liveCount(db, db.tracks), 0);
      expect(await _liveCount(db, db.clips), 0); // cascaded
      expect(await _liveCount(db, db.practiceNotes), 0);
    },
  );

  test('delete tombstones the practice; list excludes it', () async {
    final a = ok(await repo.createPractice('A'));
    await repo.createPractice('B');
    expect(
      (await repo.listPractices() as Ok<List<SongPractice>>).value,
      hasLength(2),
    );

    await repo.deletePractice(a.id);
    expect(
      (await repo.listPractices() as Ok<List<SongPractice>>).value,
      hasLength(1),
    );
    expect(await repo.getPractice(a.id), isA<Error<SongPractice>>());
  });

  // --- regression: data-loss blockers ---

  test('a clip whose pooled file goes missing survives reload + re-save '
      '(never dropped or tombstoned)', () async {
    final base = ok(await repo.createPractice('S'));
    final vocal = AudioTrack(
      id: newId(),
      kind: AudioTrackKind.vocal,
      displayName: 'Take',
      createdAt: DateTime(2026),
      versionId: base.versions.single.id,
      clips: [
        await clip('take.wav', [1, 2, 3]),
      ],
    );
    ok(
      await repo.savePractice(
        SongPractice(
          id: base.id,
          title: base.title,
          createdAt: base.createdAt,
          updatedAt: base.updatedAt,
          vocalTracks: [vocal],
          versions: base.versions,
          activeVersionId: base.versions.single.id,
        ),
      ),
    );

    // Simulate the bytes going missing locally.
    final rel =
        (await db
                .customSelect('SELECT local_rel_path AS r FROM assets LIMIT 1')
                .getSingle())
            .read<String>('r');
    await File(p.join(tmp.path, rel)).delete();

    // Reload keeps the clip (as an asset:// ref) — it must NOT be dropped.
    final reloaded = ok(await repo.getPractice(base.id));
    expect(reloaded.vocalTracks.single.clips, hasLength(1));
    expect(
      reloaded.vocalTracks.single.clips.single.audioFilePath,
      startsWith('asset://'),
    );

    // Re-saving the reloaded model must NOT tombstone the still-valid clip row.
    ok(await repo.savePractice(reloaded));
    expect(await _liveCount(db, db.clips), 1);
  });

  test('replacing the accompaniment does not roll back (tombstone before insert '
      'frees the unique slot)', () async {
    final base = ok(await repo.createPractice('S'));
    ok(
      await repo.savePractice(
        SongPractice(
          id: base.id,
          title: base.title,
          createdAt: base.createdAt,
          updatedAt: base.updatedAt,
          accompaniment: AudioTrack(
            id: newId(),
            kind: AudioTrackKind.accompaniment,
            displayName: 'B1',
            createdAt: DateTime(2026),
            clips: [
              await clip('b1.wav', [1]),
            ],
          ),
          versions: base.versions,
          activeVersionId: base.versions.single.id,
        ),
      ),
    );

    // Replace with a NEW accompaniment track (different id, same single slot).
    final result = await repo.savePractice(
      SongPractice(
        id: base.id,
        title: base.title,
        createdAt: base.createdAt,
        updatedAt: base.updatedAt,
        accompaniment: AudioTrack(
          id: newId(),
          kind: AudioTrackKind.accompaniment,
          displayName: 'B2',
          createdAt: DateTime(2026),
          clips: [
            await clip('b2.wav', [2]),
          ],
        ),
        versions: base.versions,
        activeVersionId: base.versions.single.id,
      ),
    );

    expect(
      result,
      isA<Ok<SongPractice>>(),
    ); // did NOT roll back on ux_track_slot
    expect(ok(result).accompaniment!.displayName, 'B2');
    final n =
        (await db
                .customSelect(
                  'SELECT COUNT(*) AS n FROM tracks WHERE '
                  "kind = 'accompaniment' AND deleted_at IS NULL",
                )
                .getSingle())
            .read<int>('n');
    expect(n, 1);
  });

  test('removing a vocal track journals exactly one tombstone per row whose '
      'HLC matches the row (no duplicate / stale oplog entry)', () async {
    final base = ok(await repo.createPractice('S'));
    final track = AudioTrack(
      id: newId(),
      kind: AudioTrackKind.vocal,
      displayName: 'Take',
      createdAt: DateTime(2026),
      versionId: base.versions.single.id,
      clips: [
        await clip('t.wav', [1, 2]),
      ],
    );
    ok(
      await repo.savePractice(
        SongPractice(
          id: base.id,
          title: base.title,
          createdAt: base.createdAt,
          updatedAt: base.updatedAt,
          vocalTracks: [track],
          versions: base.versions,
          activeVersionId: base.versions.single.id,
        ),
      ),
    );

    // Save without the track -> reconcile-driven cascade tombstone.
    ok(
      await repo.savePractice(
        SongPractice(
          id: base.id,
          title: base.title,
          createdAt: base.createdAt,
          updatedAt: base.updatedAt,
          versions: base.versions,
          activeVersionId: base.versions.single.id,
        ),
      ),
    );

    // Exactly ONE tracks tombstone op for this id, and its HLC equals the row's
    // (the old bug emitted a second, HLC-mismatched entry).
    final trackOps = await db
        .customSelect(
          'SELECT hlc_packed FROM oplog WHERE '
          "entity_table = 'tracks' AND entity_id = '${track.id}' "
          "AND op = 'tombstone'",
        )
        .get();
    expect(trackOps, hasLength(1));
    final rowHlc =
        (await db
                .customSelect(
                  "SELECT hlc_packed FROM tracks WHERE id = '${track.id}'",
                )
                .getSingle())
            .read<String>('hlc_packed');
    expect(trackOps.single.read<String>('hlc_packed'), rowHlc);

    // The cascaded clip is tombstoned exactly once too.
    final clipOps =
        (await db
                .customSelect(
                  'SELECT COUNT(*) AS n FROM oplog WHERE '
                  "entity_table = 'clips' AND op = 'tombstone'",
                )
                .getSingle())
            .read<int>('n');
    expect(clipOps, 1);
  });

  // Regression guard: children were reconciled for EVERY desired track, even
  // one whose remote delete the three-way merge had just accepted. A child the
  // stale editor had edited was then revived (rule 5's edited side) as a live
  // row under a tombstoned parent — invisible on this device, but synced out
  // as an orphan.
  test(
    'a track whose remote delete is accepted takes its children with it',
    () async {
      final base = ok(await repo.createPractice('S'));
      final vocal = AudioTrack(
        id: newId(),
        kind: AudioTrackKind.vocal,
        displayName: 'Take',
        createdAt: DateTime(2026),
        versionId: base.versions.single.id,
        clips: [
          await clip('t.wav', [1]),
        ],
        notes: [
          PracticeNote(
            id: newId(),
            positionMs: 0,
            content: 'x',
            createdAt: DateTime(2026),
            updatedAt: DateTime(2026),
          ),
        ],
      );
      SongPractice practiceWith(List<AudioTrack> tracks) => SongPractice(
        id: base.id,
        title: base.title,
        createdAt: base.createdAt,
        updatedAt: base.updatedAt,
        vocalTracks: tracks,
        versions: base.versions,
        activeVersionId: base.activeVersionId,
      );
      ok(await repo.savePractice(practiceWith([vocal])));

      // An editor opens the practice: these hashes are its merge base.
      final loaded =
          (await repo.loadPractice(base.id) as Ok<Loaded<SongPractice>>).value;

      // The track is deleted elsewhere and syncs in, cascading to its children.
      ok(await repo.savePractice(practiceWith(const [])));
      expect(await _liveCount(db, db.tracks), 0);

      // The stale editor saves: it never touched the TRACK (so the delete is
      // accepted) but it did edit a note underneath it.
      final held = loaded.aggregate.vocalTracks.single;
      final saved = await repo.savePracticeWithBase(
        practiceWith([
          held.copyWith(
            notes: [
              held.notes.single.copyWith(content: 'edited while deleted'),
            ],
          ),
        ]),
        baseHashes: loaded.baseHashes,
      );
      expect(saved, isA<Ok<Loaded<SongPractice>>>());
      expect(
        await _liveCount(db, db.tracks),
        0,
        reason: 'the accepted delete must stand',
      );
      expect(
        await _liveCount(db, db.practiceNotes),
        0,
        reason: 'a live note under a tombstoned track is an orphan',
      );
      expect(await _liveCount(db, db.clips), 0);
    },
  );
}

Future<int> _liveCount(AppDatabase db, TableInfo<Table, dynamic> table) async {
  final rows = await db
      .customSelect(
        'SELECT COUNT(*) AS n FROM ${table.actualTableName} WHERE deleted_at IS NULL',
      )
      .getSingle();
  return rows.data['n'] as int;
}
