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

  test('EVERY editable field round-trips (no column silently dropped)', () async {
    // The narrower round-trip above pins a representative subset. This one sets
    // every user-editable field on every row type to a distinct non-default
    // value, so a column the writer forgets — or the hydrator never reads —
    // shows up as a lost edit here instead of as "my pan reset itself".
    final base = ok(await repo.createPractice('Everything'));
    final versionId = base.versions.single.id;
    final busA = newId();
    final busB = newId();
    final originalMix = File(p.join(tmp.path, 'original-mix.wav'));
    await originalMix.writeAsBytes([4, 2]);

    final fx = TrackFx.flat().copyWith(
      eqOn: true,
      compOn: true,
      deEsserOn: true,
      sendDb: -6,
      comp: const CompSettings(thresholdDb: -18, ratio: 3.5, makeupDb: 2),
    );
    final vocal = AudioTrack(
      id: newId(),
      kind: AudioTrackKind.vocal,
      displayName: 'Everything Take',
      createdAt: DateTime(2026),
      versionId: versionId,
      volume: 0.63,
      pan: -0.42,
      muted: true,
      soloed: true,
      colorValue: 0xFF123456,
      busId: busA,
      fx: fx,
      sends: [TrackSend(busId: busB, levelDb: -9)],
      clips: [
        (await clip('a.wav', [1], peaks: [0.25])).copyWith(startMs: 250),
        (await clip('b.wav', [2], peaks: [0.5])).copyWith(startMs: 9000),
      ],
      notes: [
        PracticeNote(
          id: newId(),
          positionMs: 300,
          endMs: 1300,
          content: 'ranged note',
          createdAt: DateTime(2026),
          updatedAt: DateTime(2026),
        ),
      ],
      techniqueMarkers: [
        TechniqueMarker(
          id: newId(),
          positionMs: 400,
          endMs: 1400,
          techniqueId: 'tech-9',
          techniqueName: 'Belt',
          createdAt: DateTime(2026),
          updatedAt: DateTime(2026),
        ),
      ],
    );

    final res = await repo.savePractice(
      SongPractice(
        id: base.id,
        title: base.title,
        createdAt: base.createdAt,
        updatedAt: base.updatedAt,
        vocalTracks: [vocal],
        versions: base.versions,
        activeVersionId: versionId,
        timelineDurationMs: 44000,
        masterVolume: 0.71,
        separationSource: originalMix.path,
        reverb: const ReverbSettings(
          roomSize: 0.81,
          damping: 0.22,
          wet: 0.33,
          width: 0.44,
        ),
        fxBuses: [
          FxBus(
            id: busA,
            name: 'Lead',
            fx: fx,
            colorValue: 0xFF654321,
            volume: 0.55,
            muted: true,
            soloed: true,
            pan: 0.37,
            outputBusId: busB,
            sends: [TrackSend(busId: busB, levelDb: -12)],
          ),
          FxBus(id: busB, name: 'Verb', fx: TrackFx.flat()),
        ],
      ),
    );
    final saved = ok(res);
    expect(saved.id, base.id);

    final l = ok(await repo.getPractice(base.id));
    expect(l.masterVolume, closeTo(0.71, 1e-9));
    expect(l.timelineDurationMs, 44000);
    expect(l.separationSource, isNotNull);
    expect(await File(l.separationSource!).readAsBytes(), [4, 2]);
    expect(l.reverb?.roomSize, closeTo(0.81, 1e-9));
    expect(l.reverb?.damping, closeTo(0.22, 1e-9));
    expect(l.reverb?.wet, closeTo(0.33, 1e-9));
    expect(l.reverb?.width, closeTo(0.44, 1e-9));

    final v = l.vocalTracks.single;
    expect(v.volume, closeTo(0.63, 1e-9));
    expect(v.pan, closeTo(-0.42, 1e-9));
    expect(v.muted, isTrue);
    expect(v.soloed, isTrue);
    expect(v.colorValue, 0xFF123456);
    expect(v.busId, busA);
    expect(v.sends.single.busId, busB);
    expect(v.sends.single.levelDb, closeTo(-9, 1e-9));
    expect(v.fx?.eqOn, isTrue);
    expect(v.fx?.compOn, isTrue);
    expect(v.fx?.deEsserOn, isTrue);
    expect(v.fx?.sendDb, closeTo(-6, 1e-9));
    expect(v.fx?.comp.thresholdDb, closeTo(-18, 1e-9));
    expect(v.clips.map((c) => c.startMs), [250, 9000]);
    expect(v.notes.single.endMs, 1300);
    expect(v.techniqueMarkers.single.endMs, 1400);

    final a = l.fxBuses.firstWhere((b) => b.id == busA);
    expect(a.name, 'Lead');
    expect(a.colorValue, 0xFF654321);
    expect(a.volume, closeTo(0.55, 1e-9));
    expect(a.muted, isTrue);
    expect(a.soloed, isTrue);
    expect(a.pan, closeTo(0.37, 1e-9));
    expect(a.outputBusId, busB);
    expect(a.sends.single.busId, busB);
    expect(a.sends.single.levelDb, closeTo(-12, 1e-9));
    expect(a.fx.eqOn, isTrue);
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

  // Regression guard: the tombstone-first ordering only frees a slot when the
  // old row LEAVES the aggregate. A slot ROTATION keeps the old row in desired
  // with its kind changed — the first-separation flow re-kinds the original
  // (accompaniment -> reference) while a fresh instrumental takes the
  // accompaniment slot — and the new row's insert then collided with the old
  // row still holding the slot in the DB, rolling back the whole save.
  test('slot rotation (old accompaniment re-kinded to reference + a new '
      'accompaniment) saves without a ux_track_slot rollback', () async {
    final base = ok(await repo.createPractice('S'));
    SongPractice withSlots({
      AudioTrack? accompaniment,
      AudioTrack? reference,
    }) => SongPractice(
      id: base.id,
      title: base.title,
      createdAt: base.createdAt,
      updatedAt: base.updatedAt,
      accompaniment: accompaniment,
      reference: reference,
      versions: base.versions,
      activeVersionId: base.versions.single.id,
    );
    final originalId = newId();
    final originalClip = await clip('mix.wav', [1, 2, 3]);
    ok(
      await repo.savePractice(
        withSlots(
          accompaniment: AudioTrack(
            id: originalId,
            kind: AudioTrackKind.accompaniment,
            displayName: 'Full mix',
            createdAt: DateTime(2026),
            clips: [originalClip],
          ),
        ),
      ),
    );

    // The separation result: same id, kind rotated to reference; a NEW id
    // claims the accompaniment slot in the same save.
    final result = await repo.savePractice(
      withSlots(
        accompaniment: AudioTrack(
          id: newId(),
          kind: AudioTrackKind.accompaniment,
          displayName: 'Full mix (Instrumental)',
          createdAt: DateTime(2026),
          clips: [
            await clip('inst.wav', [4, 5]),
          ],
        ),
        reference: AudioTrack(
          id: originalId,
          kind: AudioTrackKind.reference,
          displayName: 'Full mix',
          createdAt: DateTime(2026),
          clips: [originalClip],
        ),
      ),
    );

    expect(result, isA<Ok<SongPractice>>());
    final loaded = ok(result);
    expect(loaded.accompaniment!.displayName, 'Full mix (Instrumental)');
    expect(loaded.reference!.id, originalId);
    for (final kind in ['accompaniment', 'reference']) {
      final n =
          (await db
                  .customSelect(
                    'SELECT COUNT(*) AS n FROM tracks WHERE '
                    "kind = '$kind' AND deleted_at IS NULL",
                  )
                  .getSingle())
              .read<int>('n');
      expect(n, 1, reason: 'exactly one live $kind row');
    }
  });

  test('a symmetric accompaniment/reference swap saves (no statement order '
      'can free both slots — the two-phase vacate must)', () async {
    final base = ok(await repo.createPractice('S'));
    SongPractice withSlots(AudioTrack accompaniment, AudioTrack reference) =>
        SongPractice(
          id: base.id,
          title: base.title,
          createdAt: base.createdAt,
          updatedAt: base.updatedAt,
          accompaniment: accompaniment,
          reference: reference,
          versions: base.versions,
          activeVersionId: base.versions.single.id,
        );
    final aId = newId();
    final bId = newId();
    final aClip = await clip('a.wav', [1]);
    final bClip = await clip('b.wav', [2]);
    AudioTrack track(String id, AudioTrackKind kind, AudioClip c) => AudioTrack(
      id: id,
      kind: kind,
      displayName: id == aId ? 'A' : 'B',
      createdAt: DateTime(2026),
      clips: [c],
    );
    ok(
      await repo.savePractice(
        withSlots(
          track(aId, AudioTrackKind.accompaniment, aClip),
          track(bId, AudioTrackKind.reference, bClip),
        ),
      ),
    );

    // Swap the kinds between the SAME two live rows.
    final result = await repo.savePractice(
      withSlots(
        track(bId, AudioTrackKind.accompaniment, bClip),
        track(aId, AudioTrackKind.reference, aClip),
      ),
    );

    expect(result, isA<Ok<SongPractice>>());
    final loaded = ok(result);
    expect(loaded.accompaniment!.id, bId);
    expect(loaded.reference!.id, aId);
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
