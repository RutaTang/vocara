import 'dart:convert';
import 'dart:io';

import 'package:drift/drift.dart' show Variable;
import 'package:drift/native.dart';
import 'package:flutter/foundation.dart' show mapEquals;
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/asset_store.dart';
import 'package:vocara/data/repositories/song_practice_repository_drift.dart';
import 'package:vocara/data/repositories/technique_repository_drift.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/sync_hashes.dart';
import 'package:vocara/data/sync/sync_row.dart';
import 'package:vocara/data/sync/sync_writer.dart';
import 'package:vocara/domain/models/audio_fx.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/fx_bus.dart';
import 'package:vocara/domain/models/lyric_line.dart';
import 'package:vocara/domain/models/practice_note.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/domain/models/technique_marker.dart';
import 'package:vocara/domain/models/technique_media.dart';
import 'package:vocara/utils/id.dart';
import 'package:vocara/utils/result.dart';

/// The whole base-hash scheme rests on ONE invariant: for an UNCHANGED entity,
/// the hash of the loaded row (the base/hydrate side) equals the hash the save
/// path computes from the hydrated domain aggregate. If any entity's two
/// projections drift apart, every save either writes phantom updates or —
/// worse — wrongly skips real ones. These tests prove the agreement for every
/// entity type, both directly (recompute the save-side hash from the loaded
/// aggregate and compare with the returned base) and end-to-end (a with-base
/// re-save of an untouched full graph writes NOTHING and returns an identical
/// base map).
void main() {
  late AppDatabase db;
  late Directory tmp;
  late SongPracticeRepositoryDrift songRepo;
  late TechniqueRepositoryDrift techRepo;

  setUp(() async {
    db = AppDatabase(NativeDatabase.memory());
    tmp = await Directory.systemTemp.createTemp('vocara_sync_hashes');
    final writer = SyncWriter(db, HlcClock(node: 'NODE'.padRight(26, '0')));
    final assets = AssetStore(db: db, root: tmp, writer: writer);
    songRepo = SongPracticeRepositoryDrift(
      db: db,
      writer: writer,
      assets: assets,
    );
    techRepo = TechniqueRepositoryDrift(db: db, writer: writer, assets: assets);
  });
  tearDown(() async {
    await db.close();
    if (await tmp.exists()) await tmp.delete(recursive: true);
  });

  Future<String> makeFile(String name, List<int> bytes) async {
    final f = File(p.join(tmp.path, name));
    await f.writeAsBytes(bytes);
    return f.path;
  }

  Future<int> oplogCount() async => (await db.select(db.oplog).get()).length;

  Future<String> columnOf(String table, String id, String column) async =>
      (await db
              .customSelect(
                'SELECT $column AS v FROM $table WHERE id = ?',
                variables: [Variable.withString(id)],
              )
              .getSingle())
          .read<String>('v');

  /// Saves a practice exercising EVERY reconciled entity type (practice row
  /// with reverb + lyrics + separation source, interpretations, fx bus with
  /// fx/sends, accompaniment + vocal tracks with fx/sends, clips, notes with
  /// and without a range, markers) and returns the loaded editing session.
  Future<Loaded<SongPractice>> seedFullGraph() async {
    final base =
        ((await songRepo.createPractice('Full graph')) as Ok<SongPractice>)
            .value;
    final versionId = base.versions.single.id;
    final busId = newId();

    final accompaniment = AudioTrack(
      id: newId(),
      kind: AudioTrackKind.accompaniment,
      displayName: 'Backing',
      createdAt: DateTime(2026),
      clips: [
        AudioClip(
          id: newId(),
          audioFilePath: await makeFile('backing.wav', [1, 2, 3]),
          durationMs: 4000,
          startMs: 250,
        ),
      ],
    );
    final vocal = AudioTrack(
      id: newId(),
      kind: AudioTrackKind.vocal,
      displayName: 'Take 1',
      createdAt: DateTime(2026),
      versionId: versionId,
      colorValue: 0xFF00FF00,
      muted: true,
      soloed: false,
      volume: 0.7,
      pan: -0.2,
      busId: busId,
      fx: TrackFx.flat(),
      sends: [TrackSend(busId: busId, levelDb: -6)],
      clips: [
        AudioClip(
          id: newId(),
          audioFilePath: await makeFile('take.wav', [9, 8, 7]),
          durationMs: 3000,
          startMs: 100,
        ),
      ],
      notes: [
        PracticeNote(
          id: newId(),
          positionMs: 1200,
          content: 'point note',
          createdAt: DateTime(2026),
          updatedAt: DateTime(2026),
        ),
        PracticeNote(
          id: newId(),
          positionMs: 1500,
          endMs: 2500,
          content: 'range note',
          createdAt: DateTime(2026),
          updatedAt: DateTime(2026),
        ),
      ],
      techniqueMarkers: [
        TechniqueMarker(
          id: newId(),
          positionMs: 2000,
          endMs: 2600,
          techniqueId: 'tech-1',
          techniqueName: 'Twang',
          createdAt: DateTime(2026),
          updatedAt: DateTime(2026),
        ),
      ],
    );
    final bus = FxBus(
      id: busId,
      name: 'Reverb bus',
      fx: TrackFx.flat(),
      colorValue: 0xFF112233,
      volume: 0.9,
      pan: 0.1,
      muted: false,
      soloed: true,
      outputBusId: null,
      sends: const [],
    );

    final enriched = SongPractice(
      id: base.id,
      title: 'Full graph',
      createdAt: base.createdAt,
      updatedAt: base.updatedAt,
      accompaniment: accompaniment,
      vocalTracks: [vocal],
      versions: base.versions,
      activeVersionId: versionId,
      lyrics: const [
        LyricLine(text: 'line one', timeMs: 0),
        LyricLine(text: 'line two'),
      ],
      timelineDurationMs: 30000,
      separationSource: await makeFile('mix.wav', [4, 4, 4, 4]),
      reverb: const ReverbSettings(roomSize: 0.6, wet: 0.3),
      masterVolume: 0.8,
      fxBuses: [bus],
    );
    final saved = await songRepo.savePractice(enriched);
    expect(saved, isA<Ok<SongPractice>>());
    final loaded = await songRepo.loadPractice(base.id);
    return (loaded as Ok<Loaded<SongPractice>>).value;
  }

  test('every practice entity type: the loaded-row base hash equals the '
      'save-side hash recomputed from the hydrated aggregate', () async {
    final session = await seedFullGraph();
    final practice = session.aggregate;
    final baseHashes = session.baseHashes;

    // --- practice row (JSON columns re-encoded exactly like the save path;
    // the separation asset id is store-assigned, read from the row).
    expect(
      baseHashes[practice.id],
      practiceContentHash(
        title: practice.title,
        timelineDurationMs: practice.timelineDurationMs,
        masterVolume: practice.masterVolume,
        activeInterpretationId: practice.activeVersionId,
        separationAssetId: await columnOf(
          'practices',
          practice.id,
          'separation_asset_id',
        ),
        reverbJson: jsonEncode(practice.reverb!.toJson()),
        lyricsJson: jsonEncode(practice.lyrics.map((l) => l.toJson()).toList()),
      ),
      reason: 'practices projection must agree',
    );

    // --- interpretations (order index = list position, like the save).
    for (var i = 0; i < practice.versions.length; i++) {
      final v = practice.versions[i];
      expect(
        baseHashes[v.id],
        interpretationContentHash(name: v.name, orderIndex: i),
        reason: 'interpretations projection must agree',
      );
    }

    // --- tracks, flattened in save order (accompaniment, reference, vocals).
    final allTracks = <AudioTrack>[
      if (practice.accompaniment != null) practice.accompaniment!,
      if (practice.reference != null) practice.reference!,
      ...practice.vocalTracks,
    ];
    for (var i = 0; i < allTracks.length; i++) {
      final t = allTracks[i];
      expect(
        baseHashes[t.id],
        trackContentHash(
          kind: t.kind.name,
          interpretationId: t.versionId,
          displayName: t.displayName,
          colorValue: t.colorValue,
          muted: t.muted,
          soloed: t.soloed,
          volume: t.volume,
          pan: t.pan,
          busId: t.busId,
          fxJson: t.fx == null ? null : jsonEncode(t.fx!.toJson()),
          sendsJson: jsonEncode(t.sends.map((s) => s.toJson()).toList()),
          orderIndex: i,
        ),
        reason: 'tracks projection must agree (${t.displayName})',
      );

      // --- clips of this track (the asset id is store-assigned).
      for (var c = 0; c < t.clips.length; c++) {
        final clip = t.clips[c];
        expect(
          baseHashes[clip.id],
          clipContentHash(
            assetId: await columnOf('clips', clip.id, 'asset_id'),
            startMs: clip.startMs,
            durationMs: clip.durationMs,
            orderIndex: c,
          ),
          reason: 'clips projection must agree',
        );
      }

      // --- notes (point and range) and markers.
      for (final n in t.notes) {
        expect(
          baseHashes[n.id],
          practiceNoteContentHash(
            positionMs: n.positionMs,
            endMs: n.endMs,
            content: n.content,
          ),
          reason: 'practice_notes projection must agree',
        );
      }
      for (final m in t.techniqueMarkers) {
        expect(
          baseHashes[m.id],
          techniqueMarkerContentHash(
            positionMs: m.positionMs,
            endMs: m.endMs,
            techniqueId: m.techniqueId,
            techniqueName: m.techniqueName,
          ),
          reason: 'technique_markers projection must agree',
        );
      }
    }

    // --- fx buses.
    for (var i = 0; i < practice.fxBuses.length; i++) {
      final b = practice.fxBuses[i];
      expect(
        baseHashes[b.id],
        fxBusContentHash(
          name: b.name,
          colorValue: b.colorValue,
          volume: b.volume,
          pan: b.pan,
          muted: b.muted,
          soloed: b.soloed,
          outputBusId: b.outputBusId,
          fxJson: jsonEncode(b.fx.toJson()),
          sendsJson: jsonEncode(b.sends.map((s) => s.toJson()).toList()),
          orderIndex: i,
        ),
        reason: 'fx_buses projection must agree',
      );
    }

    // Every reconciled row is covered: practice + 1 interpretation +
    // 2 tracks + 2 clips + 2 notes + 1 marker + 1 bus.
    expect(baseHashes.length, 10);
  });

  test('END-TO-END: a with-base re-save of an untouched full practice graph '
      'writes nothing and returns an identical base map', () async {
    final session = await seedFullGraph();
    final before = await oplogCount();

    final again = await songRepo.savePracticeWithBase(
      session.aggregate,
      baseHashes: session.baseHashes,
    );
    expect(again, isA<Ok<Loaded<SongPractice>>>());
    expect(
      await oplogCount(),
      before,
      reason:
          'any new op means a load-side and a save-side projection disagree',
    );
    expect(
      mapEquals(
        (again as Ok<Loaded<SongPractice>>).value.baseHashes,
        session.baseHashes,
      ),
      isTrue,
      reason: 'an untouched graph must round-trip to identical base hashes',
    );
  });

  test('a flat baseHashes map is sound: no two synced tables can hold the '
      'same row id', () async {
    // reconcile keys BASE by bare row id across the whole aggregate, so two
    // tables sharing an id would cross-contaminate — a track's base hash
    // answering for a clip, which decides the three-way rules and can send an
    // UPDATE down the tombstone branch. That is safe only because of how ids
    // are minted: ULIDs everywhere, plus the legacy `<trackId>-c0` clip shape,
    // which carries a '-' no ULID contains. This test is the tripwire against
    // a future id grammar that breaks the assumption silently.
    await seedFullGraph();
    final seen = <String, String>{};
    for (final entry in syncedTableRegistry(db).entries) {
      final rows = await db.customSelect('SELECT id FROM "${entry.key}"').get();
      for (final r in rows) {
        final id = r.read<String>('id');
        final owner = seen[id];
        expect(
          owner,
          isNull,
          reason: 'id "$id" is in both "$owner" and "${entry.key}"',
        );
        seen[id] = entry.key;
      }
    }
    expect(seen, isNotEmpty, reason: 'the seeded graph must have rows');
  });

  test('technique + media: base hashes equal recomputed save-side hashes, and '
      'an untouched with-base re-save writes nothing', () async {
    final t = Technique(
      id: newId(),
      name: 'Belting',
      aliases: const ['Belt'],
      tags: const ['CVT', 'Effect'],
      summary: 'loud + bright',
      description: 'a description',
      cues: 'call out',
      exercises: 'hey hey hey',
      acousticSignature: 'strong H2',
      media: [
        TechniqueMedia(
          id: newId(),
          path: await makeFile('ex.wav', [1, 1, 2]),
          kind: TechniqueMediaKind.audio,
          label: 'demo',
          addedAt: DateTime(2026),
        ),
      ],
      createdAt: DateTime(2026),
      updatedAt: DateTime(2026),
    );
    expect(await techRepo.saveTechnique(t), isA<Ok<Technique>>());

    final session =
        ((await techRepo.loadTechnique(t.id)) as Ok<Loaded<Technique>>).value;
    final loaded = session.aggregate;

    // --- technique row (JSON re-encoded exactly like the save path).
    expect(
      session.baseHashes[loaded.id],
      techniqueContentHash(
        name: loaded.name,
        aliasesJson: jsonEncode(loaded.aliases),
        tagsJson: jsonEncode(loaded.tags),
        summary: loaded.summary,
        description: loaded.description,
        cues: loaded.cues,
        exercises: loaded.exercises,
        acousticSignature: loaded.acousticSignature,
      ),
      reason: 'techniques projection must agree',
    );

    // --- media rows (the asset id is store-assigned).
    for (var i = 0; i < loaded.media.length; i++) {
      final m = loaded.media[i];
      expect(
        session.baseHashes[m.id],
        techniqueMediaContentHash(
          assetId: await columnOf('technique_media', m.id, 'asset_id'),
          kind: m.isVideo ? 'video' : 'audio',
          label: m.label,
          orderIndex: i,
        ),
        reason: 'technique_media projection must agree',
      );
    }
    expect(session.baseHashes.length, 2); // technique + 1 media

    // --- end-to-end no-op.
    final before = await oplogCount();
    final again = await techRepo.saveTechniqueWithBase(
      loaded,
      baseHashes: session.baseHashes,
    );
    expect(again, isA<Ok<Loaded<Technique>>>());
    expect(await oplogCount(), before);
    expect(
      mapEquals(
        (again as Ok<Loaded<Technique>>).value.baseHashes,
        session.baseHashes,
      ),
      isTrue,
    );
  });
}
