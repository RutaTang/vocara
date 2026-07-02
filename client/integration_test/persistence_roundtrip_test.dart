import 'dart:io';
import 'dart:math';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:media_kit/media_kit.dart' hide AudioTrack;
import 'package:path/path.dart' as p;
import 'package:path_provider/path_provider.dart';
import 'package:vocara/data/db/persistence.dart';
import 'package:vocara/data/services/app_settings_service.dart';
import 'package:vocara/data/services/audio_file_service.dart';
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/data/services/source_separation_service.dart';
import 'package:vocara/data/services/spectrogram_service.dart';
import 'package:vocara/data/services/waveform_service.dart';
import 'package:vocara/domain/models/audio_fx.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/practice_note.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/domain/models/take_version.dart';
import 'package:vocara/ui/song_practice_detail/view_model/song_practice_detail_view_model.dart';
import 'package:vocara/utils/id.dart';
import 'package:vocara/utils/result.dart';

/// Writes a mono 16-bit PCM WAV sine tone at [freq] Hz and returns its path.
/// Each distinct ([freq], [seconds]) pair yields distinct bytes, so two writes
/// with the same parameters are byte-identical (drives the dedup assertions).
Future<String> _writeSineWav(
  String path, {
  int seconds = 1,
  double freq = 440,
}) async {
  const sampleRate = 44100;
  final samples = sampleRate * seconds;
  const bytesPerSample = 2;
  final dataSize = samples * bytesPerSample;
  final bytes = BytesBuilder();

  void str(String s) => bytes.add(s.codeUnits);
  void u32(int v) {
    final b = ByteData(4)..setUint32(0, v, Endian.little);
    bytes.add(b.buffer.asUint8List());
  }

  void u16(int v) {
    final b = ByteData(2)..setUint16(0, v, Endian.little);
    bytes.add(b.buffer.asUint8List());
  }

  str('RIFF');
  u32(36 + dataSize);
  str('WAVE');
  str('fmt ');
  u32(16);
  u16(1); // PCM
  u16(1); // mono
  u32(sampleRate);
  u32(sampleRate * bytesPerSample); // byte rate
  u16(bytesPerSample); // block align
  u16(16); // bits per sample
  str('data');
  u32(dataSize);

  final pcm = ByteData(dataSize);
  for (var i = 0; i < samples; i++) {
    final value = (sin(2 * pi * freq * i / sampleRate) * 0.5 * 32767).round();
    pcm.setInt16(i * 2, value, Endian.little);
  }
  bytes.add(pcm.buffer.asUint8List());

  final file = File(path);
  await file.parent.create(recursive: true);
  await file.writeAsBytes(bytes.toBytes());
  return path;
}

/// Pumps the widget tree until [until] is true or the cap is hit. Mirrors the
/// existing integration test's settle loop (no fixed sleeps).
Future<void> _settle(
  WidgetTester tester,
  bool Function() until, {
  int maxIterations = 80,
}) async {
  for (var i = 0; i < maxIterations && !until(); i++) {
    await tester.pump(const Duration(milliseconds: 100));
  }
}

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  MediaKit.ensureInitialized();

  SongPracticeDetailViewModel buildVm(Persistence persistence, String id) {
    final storage = LocalStorageService();
    return SongPracticeDetailViewModel(
      repository: persistence.songRepo,
      techniqueRepository: persistence.techRepo,
      audioFileService: AudioFileService(storage: storage),
      waveformService: WaveformService(storage: storage),
      spectrogramService: const SpectrogramService(),
      separationService: FakeSourceSeparationService(),
      appSettings: AppSettingsService(storage: storage)..load(),
      practiceId: id,
    );
  }

  testWidgets(
    'detail VM drives a full edit then every field round-trips through a '
    'fresh VM on the same drift store',
    (tester) async {
      final tmp = await getTemporaryDirectory();
      final stamp = DateTime.now().microsecondsSinceEpoch;
      final backingWav = await _writeSineWav(
        p.join(tmp.path, 'backing_$stamp.wav'),
        freq: 220,
      );
      final takeWav = await _writeSineWav(
        p.join(tmp.path, 'take_$stamp.wav'),
        freq: 440,
      );

      final persistence = Persistence.memory();
      final repo = persistence.songRepo;

      final created = await repo.createPractice('Round-trip song');
      final practiceId = (created as Ok<SongPractice>).value.id;
      final versionId = created.value.activeVersionId;
      expect(versionId, isNotNull);

      // --- Drive the VM through a representative edit session. ---
      final vm = buildVm(persistence, practiceId);
      await _settle(tester, () => vm.practice != null);
      expect(vm.practice, isNotNull, reason: 'initial load must populate');

      // Accompaniment import (real waveform analysis on a real WAV).
      await vm.addAccompaniment(backingWav);
      await _settle(
        tester,
        () => !vm.busy && vm.practice?.accompaniment != null,
      );
      final accomp = vm.practice!.accompaniment;
      expect(accomp, isNotNull);
      expect(accomp!.waveformPeaks, isNotEmpty);
      expect(accomp.durationMs, greaterThan(500));

      // A vocal take (its own lane) in the active interpretation.
      await vm.addVocalTake(takeWav);
      await _settle(tester, () => !vm.busy && vm.vocalTakes.isNotEmpty);
      expect(vm.vocalTakes, hasLength(1));
      final takeId = vm.vocalTakes.single.id;
      expect(vm.vocalTakes.single.waveformPeaks, isNotEmpty);

      // Notes at the playhead on both a backing and a vocal lane.
      await vm.seek(const Duration(milliseconds: 250));
      await _settle(tester, () => vm.position.value.inMilliseconds >= 0);
      await vm.addNoteAtPlayhead(accomp.id, 'count-in here');
      await vm.addNote(takeId, 400, 900, 'sustain this phrase');
      await _settle(tester, () => !vm.busy);
      expect(vm.practice!.accompaniment!.notes, hasLength(1));
      expect(
        vm.vocalTakes.single.notes,
        hasLength(1),
        reason: 'the take note must be on the take lane',
      );

      // Technique markers on the take (a point + a range).
      await vm.addTechnique(takeId, 'tech-twang', 'Twang', 500, null);
      await vm.addTechnique(takeId, 'tech-cry', 'Cry', 600, 1200);
      await _settle(tester, () => !vm.busy);
      expect(vm.vocalTakes.single.techniqueMarkers, hasLength(2));

      // Track FX (EQ on, a boosted band) committed. FX is addressed by track id
      // so no selection is needed.
      final editedFx = TrackFx.flat().copyWith(
        eqOn: true,
        compOn: true,
        sendDb: -6.0,
      );
      final boostedFx = editedFx.withBand(
        2,
        editedFx.eq[2].copyWith(gainDb: 4.5),
      );
      vm.setTrackFxLive(takeId, boostedFx);
      await vm.commitFx();
      await _settle(tester, () => !vm.busy);

      // Shared reverb tuned + committed.
      const reverb = ReverbSettings(
        roomSize: 0.8,
        damping: 0.3,
        wet: 0.42,
        width: 0.9,
      );
      vm.setReverbLive(reverb);
      await vm.commitFx();
      await _settle(tester, () => !vm.busy);

      // Master volume.
      vm.setMasterVolumeLive(0.66);
      await vm.commitMasterVolume();
      await _settle(tester, () => !vm.busy);

      // Lyrics.
      await vm.setLyricsText('first line\nsecond line\nthird line');
      await _settle(tester, () => vm.lyrics.isNotEmpty);
      expect(vm.lyrics, hasLength(3));

      // A second interpretation, then switch back to the first so the active
      // version is deterministic for the reload assertions.
      await vm.createInterpretation('Powerful');
      await _settle(tester, () => vm.interpretations.length == 2);
      await vm.setActiveInterpretation(versionId!);
      await _settle(tester, () => vm.activeInterpretation?.id == versionId);

      // Capture expected values before tearing the VM down.
      final expectedAccompPeaks = accomp.waveformPeaks.length;
      final expectedTakePeaks = vm.vocalTakes.single.waveformPeaks.length;
      final expectedTimeline = vm.practice!.timelineDurationMs;

      vm.dispose();

      // --- Fresh VM on the SAME store + practiceId: everything must reload. ---
      final vm2 = buildVm(persistence, practiceId);
      await _settle(tester, () => vm2.load.completed && vm2.practice != null);
      expect(vm2.practice, isNotNull, reason: 'reload must succeed');

      expect(vm2.practice!.title, 'Round-trip song');
      expect(vm2.practice!.timelineDurationMs, expectedTimeline);

      // Accompaniment.
      final a2 = vm2.practice!.accompaniment;
      expect(a2, isNotNull);
      expect(a2!.displayName, accomp.displayName);
      expect(a2.clips, hasLength(1));
      expect(a2.waveformPeaks, hasLength(expectedAccompPeaks));
      expect(a2.notes, hasLength(1));
      expect(a2.notes.single.content, 'count-in here');
      // The accompaniment clip resolves to a readable pooled file.
      final accompFile = a2.clips.single.audioFilePath;
      expect(accompFile.startsWith('asset://'), isFalse);
      expect(await File(accompFile).exists(), isTrue);

      // Active interpretation + its single take.
      expect(vm2.activeInterpretation?.id, versionId);
      expect(vm2.vocalTakes, hasLength(1));
      final take2 = vm2.vocalTakes.single;
      expect(take2.id, takeId);
      expect(take2.waveformPeaks, hasLength(expectedTakePeaks));

      // Notes round-trip on the take.
      expect(take2.notes, hasLength(1));
      final note2 = take2.notes.single;
      expect(note2.content, 'sustain this phrase');
      expect(note2.positionMs, 400);
      expect(note2.endMs, 900);

      // Technique markers round-trip (sorted by position).
      final markers = [...take2.techniqueMarkers]
        ..sort((m, n) => m.positionMs.compareTo(n.positionMs));
      expect(markers, hasLength(2));
      expect(markers[0].techniqueName, 'Twang');
      expect(markers[0].positionMs, 500);
      expect(markers[0].endMs, isNull, reason: 'point marker');
      expect(markers[1].techniqueName, 'Cry');
      expect(markers[1].positionMs, 600);
      expect(markers[1].endMs, 1200, reason: 'range marker preserved');

      // FX round-trips on the take.
      final fx2 = take2.fx;
      expect(fx2, isNotNull, reason: 'committed FX must persist');
      expect(fx2!.eqOn, isTrue);
      expect(fx2.compOn, isTrue);
      expect(fx2.sendDb, closeTo(-6.0, 1e-6));
      expect(fx2.eq[2].gainDb, closeTo(4.5, 1e-6));

      // Reverb round-trips.
      final r2 = vm2.reverbSettings;
      expect(r2.roomSize, closeTo(0.8, 1e-6));
      expect(r2.damping, closeTo(0.3, 1e-6));
      expect(r2.wet, closeTo(0.42, 1e-6));
      expect(r2.width, closeTo(0.9, 1e-6));

      // Master volume + lyrics + interpretations.
      expect(vm2.masterVolume, closeTo(0.66, 1e-6));
      expect(vm2.lyrics.map((l) => l.text).toList(), [
        'first line',
        'second line',
        'third line',
      ]);
      expect(vm2.interpretations, hasLength(2));
      expect(
        vm2.interpretations.map((v) => v.name),
        containsAll(<String>['Powerful']),
      );

      vm2.dispose();
      await persistence.db.close();
    },
  );

  test(
    'multi-clip take round-trips clip count, bytes, peaks and ordering',
    () async {
      final tmp = await Directory.systemTemp.createTemp('vocara_multiclip');
      addTearDown(() async {
        if (await tmp.exists()) await tmp.delete(recursive: true);
      });
      final persistence = Persistence.memory();
      addTearDown(persistence.db.close);

      final base =
          (await persistence.songRepo.createPractice('Multi')
                  as Ok<SongPractice>)
              .value;
      final versionId = base.versions.single.id;

      Future<AudioClip> clip(
        String name,
        List<int> bytes, {
        int startMs = 0,
        List<double> peaks = const [],
      }) async {
        final f = File(p.join(tmp.path, name));
        await f.writeAsBytes(bytes);
        return AudioClip(
          id: newId(),
          audioFilePath: f.path,
          durationMs: 5000,
          startMs: startMs,
          waveformPeaks: peaks,
        );
      }

      // A vocal lane with TWO clips on the timeline (a punch-in style take).
      // Peaks are float32-exact (powers-of-two fractions) so the content-hash
      // waveform cache round-trips them without precision loss.
      final c0 = await clip('a.wav', [1, 2, 3], peaks: const [0.5, 0.25]);
      final c1 = await clip(
        'b.wav',
        [4, 5, 6, 7],
        startMs: 6000,
        peaks: const [0.75, 0.5, 0.125],
      );
      final multi = AudioTrack(
        id: newId(),
        kind: AudioTrackKind.vocal,
        displayName: 'Two clips',
        createdAt: DateTime(2026),
        versionId: versionId,
        clips: [c0, c1],
      );

      final saved = await persistence.songRepo.savePractice(
        SongPractice(
          id: base.id,
          title: base.title,
          createdAt: base.createdAt,
          updatedAt: base.updatedAt,
          vocalTracks: [multi],
          versions: base.versions,
          activeVersionId: versionId,
        ),
      );
      expect(saved, isA<Ok<SongPractice>>());

      final loaded =
          (await persistence.songRepo.getPractice(base.id) as Ok<SongPractice>)
              .value;
      final lane = loaded.vocalTracks.single;
      expect(lane.clips, hasLength(2), reason: 'both clips persist');
      // Ordering is preserved (orderIndex), so clip[0] is the earlier one.
      expect(lane.clips[0].id, c0.id);
      expect(lane.clips[1].id, c1.id);
      expect(lane.clips[1].startMs, 6000);
      // Bytes resolve back from the content-addressed pool.
      expect(await File(lane.clips[0].audioFilePath).readAsBytes(), [1, 2, 3]);
      expect(await File(lane.clips[1].audioFilePath).readAsBytes(), [
        4,
        5,
        6,
        7,
      ]);
      // Per-clip waveform peaks survive (cached by content hash).
      expect(lane.clips[0].waveformPeaks, const [0.5, 0.25]);
      expect(lane.clips[1].waveformPeaks, const [0.75, 0.5, 0.125]);
    },
  );

  test('content-addressed assets dedup identical bytes and sweep frees only '
      'unreferenced bytes', () async {
    final tmp = await Directory.systemTemp.createTemp('vocara_assets');
    addTearDown(() async {
      if (await tmp.exists()) await tmp.delete(recursive: true);
    });
    final persistence = Persistence.memory();
    addTearDown(persistence.db.close);
    final assets = persistence.assets;

    // Importing identical bytes twice yields ONE live asset (deduped by hash).
    final srcA = File(p.join(tmp.path, 'a.wav'));
    final srcADup = File(p.join(tmp.path, 'a-copy.wav'));
    await srcA.writeAsBytes([10, 20, 30, 40]);
    await srcADup.writeAsBytes([10, 20, 30, 40]); // same content
    final idA = await assets.importFile(srcA.path);
    final idADup = await assets.importFile(srcADup.path);
    expect(idADup, idA, reason: 'identical bytes reuse the same asset id');

    final liveAfterDedup = await (persistence.db.select(
      persistence.db.assets,
    )..where((a) => a.deletedAt.isNull())).get();
    expect(
      liveAfterDedup,
      hasLength(1),
      reason: 'one asset row for one content hash',
    );

    // Wire that asset into a saved take, plus a standalone orphan asset.
    final base =
        (await persistence.songRepo.createPractice('GC') as Ok<SongPractice>)
            .value;
    final keptPath = (await assets.resolve(idA))!;
    final vocal = AudioTrack(
      id: newId(),
      kind: AudioTrackKind.vocal,
      displayName: 'Keep me',
      createdAt: DateTime(2026),
      versionId: base.versions.single.id,
      clips: [
        AudioClip(id: newId(), audioFilePath: keptPath, durationMs: 1000),
      ],
    );
    await persistence.songRepo.savePractice(
      SongPractice(
        id: base.id,
        title: base.title,
        createdAt: base.createdAt,
        updatedAt: base.updatedAt,
        vocalTracks: [vocal],
        versions: base.versions,
        activeVersionId: base.versions.single.id,
      ),
    );

    final orphan = File(p.join(tmp.path, 'orphan.wav'));
    await orphan.writeAsBytes([99, 98, 97]);
    final orphanId = await assets.importFile(orphan.path);
    final orphanPath = (await assets.resolve(orphanId))!;
    expect(await File(orphanPath).exists(), isTrue);

    // Sweep reclaims the orphan only; the referenced asset survives.
    expect(await assets.sweepUnreferenced(), 1);
    expect(await assets.resolve(orphanId), isNull);
    expect(await File(orphanPath).exists(), isFalse);
    expect(
      await assets.resolve(idA),
      isNotNull,
      reason: 'still-referenced asset survives',
    );
    expect(await File(keptPath).exists(), isTrue);

    // Drop the take (save without it) -> its asset is now unreferenced ->
    // a later sweep frees its bytes too.
    await persistence.songRepo.savePractice(
      SongPractice(
        id: base.id,
        title: base.title,
        createdAt: base.createdAt,
        updatedAt: base.updatedAt,
        versions: base.versions,
        activeVersionId: base.versions.single.id,
      ),
    );
    expect(await assets.sweepUnreferenced(), 1);
    expect(await assets.resolve(idA), isNull);
    expect(await File(keptPath).exists(), isFalse);
  });

  test(
    'interpretation switching keeps per-version takes separate across reload',
    () async {
      final tmp = await Directory.systemTemp.createTemp('vocara_versions');
      addTearDown(() async {
        if (await tmp.exists()) await tmp.delete(recursive: true);
      });
      final persistence = Persistence.memory();
      addTearDown(persistence.db.close);
      final repo = persistence.songRepo;

      final base =
          (await repo.createPractice('Versions') as Ok<SongPractice>).value;
      final v1 = base.versions.single;
      final v2 = TakeVersion(
        id: newId(),
        name: 'Interpretation 2',
        createdAt: DateTime(2026),
      );

      Future<AudioClip> clip(String name, List<int> bytes) async {
        final f = File(p.join(tmp.path, name));
        await f.writeAsBytes(bytes);
        return AudioClip(id: newId(), audioFilePath: f.path, durationMs: 1000);
      }

      final takeV1 = AudioTrack(
        id: newId(),
        kind: AudioTrackKind.vocal,
        displayName: 'V1 take',
        createdAt: DateTime(2026),
        versionId: v1.id,
        clips: [
          await clip('v1.wav', [1, 1, 1]),
        ],
        notes: [
          PracticeNote(
            id: newId(),
            positionMs: 100,
            content: 'v1 note',
            createdAt: DateTime(2026),
            updatedAt: DateTime(2026),
          ),
        ],
      );
      final takeV2a = AudioTrack(
        id: newId(),
        kind: AudioTrackKind.vocal,
        displayName: 'V2 take A',
        createdAt: DateTime(2026),
        versionId: v2.id,
        clips: [
          await clip('v2a.wav', [2, 2, 2]),
        ],
      );
      final takeV2b = AudioTrack(
        id: newId(),
        kind: AudioTrackKind.vocal,
        displayName: 'V2 take B',
        createdAt: DateTime(2026),
        versionId: v2.id,
        clips: [
          await clip('v2b.wav', [3, 3, 3]),
        ],
      );

      await repo.savePractice(
        SongPractice(
          id: base.id,
          title: base.title,
          createdAt: base.createdAt,
          updatedAt: base.updatedAt,
          vocalTracks: [takeV1, takeV2a, takeV2b],
          versions: [v1, v2],
          activeVersionId: v2.id,
        ),
      );

      final loaded =
          (await repo.getPractice(base.id) as Ok<SongPractice>).value;
      expect(loaded.versions, hasLength(2));
      expect(loaded.activeVersionId, v2.id);
      // All three takes persist...
      expect(loaded.vocalTracks, hasLength(3));
      // ...but each interpretation only sees its own.
      final v2Takes = loaded.activeVocalTracks;
      expect(v2Takes, hasLength(2));
      expect(
        v2Takes.map((t) => t.displayName),
        containsAll(<String>['V2 take A', 'V2 take B']),
      );
      expect(v2Takes.every((t) => t.versionId == v2.id), isTrue);

      // The v1 take (and its note) is still there, tagged to v1 — invisible to
      // the active v2 view but recoverable by switching.
      final v1Takes = [
        for (final t in loaded.vocalTracks)
          if (t.versionId == v1.id) t,
      ];
      expect(v1Takes, hasLength(1));
      expect(v1Takes.single.displayName, 'V1 take');
      expect(v1Takes.single.notes.single.content, 'v1 note');
    },
  );
}
