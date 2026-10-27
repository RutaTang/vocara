import 'dart:convert';
import 'dart:io';

import 'package:drift/drift.dart' show Table, TableInfo, Variable;
import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/asset_store.dart';
import 'package:vocara/data/repositories/song_practice_repository_drift.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/integrity_checker.dart';
import 'package:vocara/data/sync/quarantine_store.dart';
import 'package:vocara/data/sync/sync_hashes.dart';
import 'package:vocara/data/sync/sync_writer.dart';
import 'package:vocara/domain/models/audio_fx.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/fx_bus.dart';
import 'package:vocara/domain/models/lyric_line.dart';
import 'package:vocara/domain/models/practice_note.dart';
import 'package:vocara/domain/models/practice_summary.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/domain/models/take_version.dart';
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

  test(
    'the summary read reports what summarizing the full list would',
    () async {
      // Home reads summaries instead of hydrating every practice (every clip's
      // peaks, a file check per clip) on each return to it; the counts must
      // still be exactly what the hydrated aggregate gives.
      final now = DateTime(2026);
      PracticeNote note() => PracticeNote(
        id: newId(),
        positionMs: 0,
        content: 'n',
        createdAt: now,
        updatedAt: now,
      );
      AudioTrack track(AudioTrackKind kind, {String? version, int notes = 0}) =>
          AudioTrack(
            id: newId(),
            kind: kind,
            displayName: kind.name,
            createdAt: now,
            versionId: version,
            notes: [for (var i = 0; i < notes; i++) note()],
          );
      final empty = ok(await repo.createPractice('Empty'));
      final full = ok(await repo.createPractice('Full'));
      final v2 = TakeVersion(id: newId(), name: 'Second', createdAt: now);
      final doomed = track(AudioTrackKind.vocal, notes: 3);
      final saved = ok(
        await repo.savePractice(
          full.copyWith(
            versions: [...full.versions, v2],
            accompaniment: track(AudioTrackKind.accompaniment, notes: 1),
            reference: track(AudioTrackKind.reference, notes: 2),
            vocalTracks: [
              track(AudioTrackKind.vocal, version: full.activeVersionId),
              track(AudioTrackKind.vocal, version: v2.id, notes: 4),
              doomed,
            ],
          ),
        ),
      );
      // A tombstoned take (and its notes) and a tombstoned note count for
      // neither; nor does a deleted practice.
      await db.customStatement(
        'UPDATE tracks SET deleted_at = 1 WHERE id = ?',
        [doomed.id],
      );
      await db.customStatement(
        'UPDATE practice_notes SET deleted_at = 1 WHERE id = ?',
        [saved.vocalTracks[1].notes.first.id],
      );
      await repo.deletePractice(ok(await repo.createPractice('Gone')).id);

      final hydrated = (await repo.listPractices() as Ok<List<SongPractice>>)
          .value
          .map(PracticeSummary.of);
      final summaries =
          (await repo.listPracticeSummaries() as Ok<List<PracticeSummary>>)
              .value;
      String show(PracticeSummary p) =>
          '${p.id} ${p.title} ${p.updatedAt.millisecondsSinceEpoch} '
          '${p.takeCount} ${p.hasAccompaniment} ${p.noteCount}';
      expect(summaries.map(show), hydrated.map(show));
      expect(summaries.map((p) => p.id), [saved.id, empty.id]);
      expect(show(summaries.first), endsWith(' 2 true 6'));

      // One hydration cannot read is still LISTED (deliberately: it can be
      // deleted from Home), where the hydrated list drops it.
      final corrupt = ok(await repo.createPractice('Corrupt'));
      await db.customStatement(
        "UPDATE practices SET lyrics_json = '{not json' WHERE id = ?",
        [corrupt.id],
      );
      final listed = (await repo.listPractices() as Ok<List<SongPractice>>)
          .value
          .map((p) => p.id);
      expect(listed, isNot(contains(corrupt.id)));
      final summarized =
          (await repo.listPracticeSummaries() as Ok<List<PracticeSummary>>)
              .value
              .map((p) => p.id);
      expect(summarized, contains(corrupt.id));
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

  test('an edit or delete over a row stamped ahead of this clock orders '
      'after it', () async {
    // A second running instance shares the library with a clock of its own,
    // so a row it wrote can carry a stamp ahead of ours. Stamped from our
    // clock alone, the edit replacing it lost entity-level LWW to the very
    // version it replaced.
    final pr = ok(await repo.createPractice('Mine'));
    final versionId = pr.versions.single.id;
    final ahead = Hlc(
      wallMs: DateTime.now().millisecondsSinceEpoch + 60000,
      counter: 7,
      node: 'OTHER'.padRight(26, '0'),
    ).packed;
    Future<void> plant(String table, String id) => db.customUpdate(
      'UPDATE $table SET hlc_packed = ? WHERE id = ?',
      variables: [Variable.withString(ahead), Variable.withString(id)],
    );
    Future<String> held(String table, String id) async =>
        (await db
                .customSelect(
                  'SELECT hlc_packed FROM $table WHERE id = ?',
                  variables: [Variable.withString(id)],
                )
                .getSingle())
            .read<String>('hlc_packed');

    await plant('practices', pr.id);
    await plant('interpretations', versionId);
    final saved = ok(
      await repo.savePractice(
        pr.copyWith(
          title: 'Renamed',
          versions: [
            for (final v in pr.versions) v.copyWith(name: 'Renamed take'),
          ],
        ),
      ),
    );
    expect(saved.title, 'Renamed');
    expect((await held('practices', pr.id)).compareTo(ahead), 1);
    expect((await held('interpretations', versionId)).compareTo(ahead), 1);

    await plant('practices', pr.id);
    await plant('interpretations', versionId);
    await repo.deletePractice(pr.id);
    expect((await held('practices', pr.id)).compareTo(ahead), 1);
    expect((await held('interpretations', versionId)).compareTo(ahead), 1);
    // The journal carries the same order.
    final last = await (db.select(
      db.oplog,
    )..where((o) => o.entityId.equals(pr.id))).get();
    expect(last.last.hlcPacked.compareTo(ahead), 1);
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

  test('a save that touches no audio keeps every clip on its asset, even when '
      'a pooled file changed after pooling', () async {
    final base = ok(await repo.createPractice('S'));
    final vocal = AudioTrack(
      id: newId(),
      kind: AudioTrackKind.vocal,
      displayName: 'Take',
      createdAt: DateTime(2026),
      versionId: base.versions.single.id,
      clips: [
        await clip('take.wav', [1, 2, 3, 4]),
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
    final reloaded = ok(await repo.getPractice(base.id));
    final assetBefore = (await db.select(db.clips).getSingle()).assetId;

    // The clip comes back carrying its pool path; flip a byte under it (rot /
    // an outside edit — verifyPoolIntegrity's report-only case).
    final pooled = reloaded.vocalTracks.single.clips.single.audioFilePath;
    await File(pooled).writeAsBytes([1, 2, 3, 5], flush: true);

    final opsBefore = (await db.select(db.oplog).get()).length;
    ok(await repo.savePractice(reloaded.copyWith(title: 'Renamed')));

    // A rename must not mint an asset for the changed bytes and re-point the
    // clip at it (journaled, that would ship the bad bytes fleet-wide).
    expect((await db.select(db.clips).getSingle()).assetId, assetBefore);
    expect(await db.select(db.assets).get(), hasLength(1));
    final ops = (await db.select(db.oplog).get()).skip(opsBefore);
    expect(ops.map((o) => '${o.entityTable}:${o.op}'), ['practices:update']);
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

  group('a take whose interpretation resolves to none', () {
    // Sync leaves such takes: the integrity repair NULLs a pointer at an
    // interpretation another device deleted, and one whose newer state is
    // parked in the quarantine is not here yet. The take is LISTED under the
    // active interpretation, but loading rewrites nothing: a load-time
    // rewrite was an edit nobody made, whose next save — any save — outranked
    // other devices' concurrent changes and the parked interpretation's own.
    final now = DateTime(2026);
    AudioTrack take(String name, String versionId) => AudioTrack(
      id: newId(),
      kind: AudioTrackKind.vocal,
      displayName: name,
      createdAt: now,
      versionId: versionId,
    );
    Future<TrackRow> trackRow(String id) =>
        (db.select(db.tracks)..where((t) => t.id.equals(id))).getSingle();
    Future<void> repair() => db.transaction(
      () => IntegrityChecker(
        db,
        SyncWriter(db, HlcClock(node: 'FIX'.padRight(26, '0'))),
      ).repair(),
    );

    /// Two interpretations; the ACTIVE one is deleted by another device and
    /// the repair NULLs both pointers at it.
    Future<({SongPractice pr, AudioTrack orphan, AudioTrack other})>
    deletedUnderIt() async {
      final keep = TakeVersion(id: newId(), name: 'Keep', createdAt: now);
      final gone = TakeVersion(id: newId(), name: 'Gone', createdAt: now);
      final orphan = take('Take 1', gone.id);
      final other = take('Take 2', keep.id);
      final pr = ok(
        await repo.savePractice(
          SongPractice(
            id: newId(),
            title: 'Song',
            createdAt: now,
            updatedAt: now,
            versions: [keep, gone],
            activeVersionId: gone.id,
            vocalTracks: [orphan, other],
          ),
        ),
      );
      await db.customStatement(
        'UPDATE interpretations SET deleted_at = 1 WHERE id = ?',
        [gone.id],
      );
      await repair();
      expect((await trackRow(orphan.id)).interpretationId, isNull);
      return (pr: pr, orphan: orphan, other: other);
    }

    Future<Loaded<SongPractice>> load(String id) async =>
        (await repo.loadPractice(id) as Ok<Loaded<SongPractice>>).value;

    /// Saves [loaded] with only the OTHER take's volume changed.
    Future<void> unrelatedSave(Loaded<SongPractice> loaded, String otherId) =>
        repo.savePracticeWithBase(
          loaded.aggregate.copyWith(
            vocalTracks: [
              for (final t in loaded.aggregate.vocalTracks)
                t.id == otherId ? t.copyWith(volume: 0.3) : t,
            ],
          ),
          baseHashes: loaded.baseHashes,
        );

    test('is listed under the active interpretation, and armable', () async {
      final f = await deletedUnderIt();
      final got = (await load(f.pr.id)).aggregate;
      expect(got.activeVocalTracks.map((t) => t.id), [f.orphan.id, f.other.id]);
      final orphan = got.vocalTracks.firstWhere((t) => t.id == f.orphan.id);
      expect(got.listsTake(orphan), isTrue);
      expect(orphan.versionId, isNull, reason: 'loaded verbatim');
    });

    test('a remote delete of it survives an unrelated save', () async {
      final f = await deletedUnderIt();
      final loaded = await load(f.pr.id);
      await db.customStatement(
        "UPDATE tracks SET deleted_at = 5, hlc_packed = 'zzzz' WHERE id = ?",
        [f.orphan.id],
      );
      await unrelatedSave(loaded, f.other.id);
      expect((await trackRow(f.orphan.id)).deletedAt, isNotNull);
    });

    test('remote edits of it and of the practice survive an unrelated '
        'save', () async {
      final f = await deletedUnderIt();
      final loaded = await load(f.pr.id);
      await db.customStatement(
        "UPDATE tracks SET display_name = 'Remote name', hlc_packed = 'zzzz' "
        'WHERE id = ?',
        [f.orphan.id],
      );
      await db.customStatement(
        "UPDATE practices SET title = 'Remote title', hlc_packed = 'zzzz' "
        'WHERE id = ?',
        [f.pr.id],
      );
      await unrelatedSave(loaded, f.other.id);
      expect((await trackRow(f.orphan.id)).displayName, 'Remote name');
      final practice = await (db.select(
        db.practices,
      )..where((x) => x.id.equals(f.pr.id))).getSingle();
      expect(practice.title, 'Remote title');
    });

    test('a take whose interpretation is PARKED stays its own when the park '
        'lands', () async {
      final keep = TakeVersion(id: newId(), name: 'Keep', createdAt: now);
      final x = TakeVersion(id: newId(), name: 'X', createdAt: now);
      final onX = take('Take on X', x.id);
      final other = take('Take on Keep', keep.id);
      final pr = ok(
        await repo.savePractice(
          SongPractice(
            id: newId(),
            title: 'Song',
            createdAt: now,
            updatedAt: now,
            versions: [keep, x],
            activeVersionId: keep.id,
            vocalTracks: [onX, other],
          ),
        ),
      );
      // X is tombstoned here, but a NEWER live state of it is parked (an edit
      // that beat the delete, held by the drift gate): the repair defers.
      await db.customStatement(
        'UPDATE interpretations SET deleted_at = 1 WHERE id = ?',
        [x.id],
      );
      final held = await db
          .customSelect(
            'SELECT hlc_packed FROM interpretations WHERE id = ?',
            variables: [Variable.withString(x.id)],
          )
          .getSingle();
      final base = Hlc.parse(held.read<String>('hlc_packed'));
      await QuarantineStore(db).put(
        table: 'interpretations',
        id: x.id,
        hlcPacked: Hlc(
          wallMs: base.wallMs + 1000,
          counter: 0,
          node: 'PEER'.padRight(26, '0'),
        ).packed,
        schemaV: 1,
        payload: '{}',
        reason: 'hlc-drift: parked by the test',
        quarantinedAt: 0,
      );
      await repair();
      expect((await trackRow(onX.id)).interpretationId, x.id);

      final loaded = await load(pr.id);
      expect(
        loaded.aggregate.activeVocalTracks.map((t) => t.id),
        [onX.id, other.id],
        reason: 'listed meanwhile',
      );
      await unrelatedSave(loaded, other.id);
      expect((await trackRow(onX.id)).interpretationId, x.id);
    });
  });

  group('a backing lane coming or going', () {
    // A take's order_index counted the accompaniment and reference slots in
    // front of it, so importing or removing a backing renumbered every take:
    // each was rewritten under a fresh stamp as an edit nobody made, and a
    // take another device had changed meanwhile was reverted to this
    // editor's stale copy (both sides "changed", the active editor wins).
    final now = DateTime(2026);
    Future<({SongPractice pr, List<AudioTrack> takes})> threeTakes() async {
      final base = ok(await repo.createPractice('S'));
      final takes = [
        for (var i = 1; i <= 3; i++)
          AudioTrack(
            id: newId(),
            kind: AudioTrackKind.vocal,
            displayName: 'Take $i',
            createdAt: now,
            versionId: base.versions.single.id,
          ),
      ];
      final pr = ok(await repo.savePractice(base.copyWith(vocalTracks: takes)));
      return (pr: pr, takes: takes);
    }

    Future<Loaded<SongPractice>> load(String id) async =>
        (await repo.loadPractice(id) as Ok<Loaded<SongPractice>>).value;
    AudioTrack backing(AudioTrackKind kind) => AudioTrack(
      id: newId(),
      kind: kind,
      displayName: kind.name,
      createdAt: now,
    );
    Future<int> takeUpdates(List<AudioTrack> takes) async =>
        (await db
                .customSelect(
                  'SELECT COUNT(*) AS n FROM oplog WHERE '
                  "entity_table = 'tracks' AND op = 'update' AND entity_id IN "
                  '(${[for (final t in takes) "'${t.id}'"].join(', ')})',
                )
                .getSingle())
            .read<int>('n');

    test('leaves every take row as it was', () async {
      final f = await threeTakes();
      var loaded = await load(f.pr.id);
      final added = await repo.savePracticeWithBase(
        loaded.aggregate.copyWith(
          accompaniment: backing(AudioTrackKind.accompaniment),
          reference: backing(AudioTrackKind.reference),
        ),
        baseHashes: loaded.baseHashes,
      );
      loaded = (added as Ok<Loaded<SongPractice>>).value;
      expect(await takeUpdates(f.takes), 0, reason: 'backings added');
      ok(
        await repo.savePractice(
          loaded.aggregate.copyWith(
            clearAccompaniment: true,
            clearReference: true,
          ),
        ),
      );
      expect(await takeUpdates(f.takes), 0, reason: 'backings removed');
      expect((await load(f.pr.id)).aggregate.vocalTracks.map((t) => t.id), [
        for (final t in f.takes) t.id,
      ]);
    });

    test("keeps another device's edit of a take it never touched", () async {
      final f = await threeTakes();
      final loaded = await load(f.pr.id);
      await db.customStatement(
        "UPDATE tracks SET display_name = 'Remote name', hlc_packed = 'zzzz' "
        'WHERE id = ?',
        [f.takes[1].id],
      );
      final saved = await repo.savePracticeWithBase(
        loaded.aggregate.copyWith(reference: backing(AudioTrackKind.reference)),
        baseHashes: loaded.baseHashes,
      );
      expect(saved, isA<Ok<Loaded<SongPractice>>>());
      final row = await (db.select(
        db.tracks,
      )..where((t) => t.id.equals(f.takes[1].id))).getSingle();
      expect(row.displayName, 'Remote name');
    });

    test('a practice numbered with the backings in front keeps its take '
        'order', () async {
      // What every earlier build wrote: accompaniment 0, reference 1, then
      // the takes. Their order here is deliberately not their id order.
      final f = await threeTakes();
      ok(
        await repo.savePractice(
          f.pr.copyWith(
            accompaniment: backing(AudioTrackKind.accompaniment),
            reference: backing(AudioTrackKind.reference),
          ),
        ),
      );
      final legacy = [f.takes[2], f.takes[0], f.takes[1]];
      for (var i = 0; i < legacy.length; i++) {
        await db.customStatement(
          'UPDATE tracks SET order_index = ? WHERE id = ?',
          [i + 2, legacy[i].id],
        );
      }
      await db.customStatement(
        "UPDATE tracks SET order_index = 1 WHERE kind = 'reference'",
      );
      final loaded = await load(f.pr.id);
      expect(loaded.aggregate.vocalTracks.map((t) => t.id), [
        for (final t in legacy) t.id,
      ]);
      final fourth = AudioTrack(
        id: newId(),
        kind: AudioTrackKind.vocal,
        displayName: 'Take 4',
        createdAt: now,
        versionId: f.pr.versions.single.id,
      );
      final saved = await repo.savePracticeWithBase(
        loaded.aggregate.copyWith(
          vocalTracks: [...loaded.aggregate.vocalTracks, fourth],
        ),
        baseHashes: loaded.baseHashes,
      );
      expect(saved, isA<Ok<Loaded<SongPractice>>>());
      expect((await load(f.pr.id)).aggregate.vocalTracks.map((t) => t.id), [
        for (final t in legacy) t.id,
        fourth.id,
      ]);
    });
  });

  group('a row whose stored JSON this build reads differently', () {
    // Hydration decodes the JSON columns into the model (clamping values out
    // of range, dropping keys it doesn't know), but the base hash was the
    // stored string's. Any save then saw such a row as edited: it rewrote it
    // as a change nobody made, stripping what another build had written, and
    // a concurrent edit of that row by another device was reverted.
    final now = DateTime(2026);
    Future<Loaded<SongPractice>> load(String id) async =>
        (await repo.loadPractice(id) as Ok<Loaded<SongPractice>>).value;
    Future<int> ops(String table, String id) async =>
        (await db
                .customSelect(
                  'SELECT COUNT(*) AS n FROM oplog '
                  'WHERE entity_table = ? AND entity_id = ?',
                  variables: [
                    Variable.withString(table),
                    Variable.withString(id),
                  ],
                )
                .getSingle())
            .read<int>('n');
    Future<String?> column(String table, String col, String id) async =>
        (await db
                .customSelect(
                  'SELECT $col AS v FROM $table WHERE id = ?',
                  variables: [Variable.withString(id)],
                )
                .getSingle())
            .read<String?>('v');

    test('the practice row: reverb and lyrics', () async {
      final base = ok(await repo.createPractice('S'));
      final take = AudioTrack(
        id: newId(),
        kind: AudioTrackKind.vocal,
        displayName: 'Take 1',
        createdAt: now,
        versionId: base.versions.single.id,
      );
      final pr = ok(
        await repo.savePractice(base.copyWith(vocalTracks: [take])),
      );
      final reverb = jsonEncode({
        ...const ReverbSettings().toJson(),
        'roomSize': 1.5,
      });
      final lyrics = jsonEncode([
        {'text': 'la', 'timeMs': 10, 'style': 'bold'},
      ]);
      await db.customStatement(
        'UPDATE practices SET reverb_json = ?, lyrics_json = ? WHERE id = ?',
        [reverb, lyrics, pr.id],
      );
      final loaded = await load(pr.id);
      final before = await ops('practices', pr.id);
      await db.customStatement(
        "UPDATE practices SET title = 'Remote title', hlc_packed = 'zzzz' "
        'WHERE id = ?',
        [pr.id],
      );
      final saved = await repo.savePracticeWithBase(
        loaded.aggregate.copyWith(
          vocalTracks: [loaded.aggregate.vocalTracks.single.copyWith(pan: 0.5)],
        ),
        baseHashes: loaded.baseHashes,
      );
      expect(saved, isA<Ok<Loaded<SongPractice>>>());
      expect(await column('practices', 'title', pr.id), 'Remote title');
      expect(await column('practices', 'reverb_json', pr.id), reverb);
      expect(await column('practices', 'lyrics_json', pr.id), lyrics);
      expect(await ops('practices', pr.id), before);

      // A real edit of the row is still written, as the model holds it.
      final next = (saved as Ok<Loaded<SongPractice>>).value;
      final edited = next.aggregate.copyWith(
        reverb: next.aggregate.reverb!.copyWith(wet: 0.5),
      );
      expect(
        await repo.savePracticeWithBase(edited, baseHashes: next.baseHashes),
        isA<Ok<Loaded<SongPractice>>>(),
      );
      expect(
        await column('practices', 'reverb_json', pr.id),
        jsonEncode(edited.reverb!.toJson()),
      );
      expect(await ops('practices', pr.id), before + 1);
    });

    test('a take and an FX bus: their chains and sends', () async {
      final base = ok(await repo.createPractice('S'));
      final bus = FxBus(id: newId(), name: 'Bus', fx: TrackFx.flat());
      AudioTrack take(String name) => AudioTrack(
        id: newId(),
        kind: AudioTrackKind.vocal,
        displayName: name,
        createdAt: now,
        versionId: base.versions.single.id,
        fx: TrackFx.flat(),
        sends: [TrackSend(busId: bus.id, levelDb: -9)],
      );
      final held = take('Take 1');
      final other = take('Take 2');
      final pr = ok(
        await repo.savePractice(
          base.copyWith(vocalTracks: [held, other], fxBuses: [bus]),
        ),
      );
      final fx = jsonEncode({...TrackFx.flat().toJson(), 'future': 1});
      final sends = jsonEncode([
        {...TrackSend(busId: bus.id, levelDb: -9).toJson(), 'future': 1},
      ]);
      await db.customStatement(
        'UPDATE tracks SET fx_json = ?, sends_json = ? WHERE id = ?',
        [fx, sends, held.id],
      );
      await db.customStatement('UPDATE fx_buses SET fx_json = ? WHERE id = ?', [
        fx,
        bus.id,
      ]);
      final loaded = await load(pr.id);
      final heldOps = await ops('tracks', held.id);
      final busOps = await ops('fx_buses', bus.id);
      await db.customStatement(
        "UPDATE tracks SET display_name = 'Remote name', hlc_packed = 'zzzz' "
        'WHERE id = ?',
        [held.id],
      );
      final saved = await repo.savePracticeWithBase(
        loaded.aggregate.copyWith(
          vocalTracks: [
            for (final t in loaded.aggregate.vocalTracks)
              t.id == other.id ? t.copyWith(volume: 0.3) : t,
          ],
        ),
        baseHashes: loaded.baseHashes,
      );
      expect(saved, isA<Ok<Loaded<SongPractice>>>());
      expect(await column('tracks', 'display_name', held.id), 'Remote name');
      expect(await column('tracks', 'fx_json', held.id), fx);
      expect(await column('tracks', 'sends_json', held.id), sends);
      expect(await column('fx_buses', 'fx_json', bus.id), fx);
      expect(await ops('tracks', held.id), heldOps);
      expect(await ops('fx_buses', bus.id), busOps);
    });
  });
}

Future<int> _liveCount(AppDatabase db, TableInfo<Table, dynamic> table) async {
  final rows = await db
      .customSelect(
        'SELECT COUNT(*) AS n FROM ${table.actualTableName} WHERE deleted_at IS NULL',
      )
      .getSingle();
  return rows.data['n'] as int;
}
