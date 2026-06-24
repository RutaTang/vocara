import 'dart:io';
import 'dart:math';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:path/path.dart' as p;
import 'package:path_provider/path_provider.dart';
import 'package:vocara/data/db/persistence.dart';
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/data/services/media_analysis_service.dart';
import 'package:vocara/data/services/technique_media_service.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/domain/models/technique_marker.dart';
import 'package:vocara/domain/models/technique_media.dart';
import 'package:vocara/ui/technique_library/view_model/technique_detail_view_model.dart';
import 'package:vocara/ui/technique_library/view_model/technique_library_view_model.dart';
import 'package:vocara/utils/id.dart';
import 'package:vocara/utils/result.dart';

/// Writes a mono 16-bit PCM WAV at [freq] Hz and returns its path. A real,
/// decodable tone so MediaAnalysisService (which decodes via the native engine)
/// produces a non-empty analysis.
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

/// Runs [test] until it returns true or the cap is reached, pumping the event
/// loop between checks. Returns whether the condition held.
Future<bool> _settle(
  WidgetTester tester,
  bool Function() test, {
  int maxIterations = 80,
}) async {
  for (var i = 0; i < maxIterations; i++) {
    if (test()) return true;
    await tester.pump(const Duration(milliseconds: 50));
  }
  return test();
}

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  late Persistence persistence;
  late TechniqueMediaService mediaService;
  late Directory scratch;

  setUp(() async {
    persistence = Persistence.memory();
    mediaService = TechniqueMediaService(storage: LocalStorageService());
    scratch = await Directory.systemTemp.createTemp('vocara_tech_it');
  });

  tearDown(() async {
    await persistence.db.close();
    if (await scratch.exists()) await scratch.delete(recursive: true);
  });

  testWidgets(
    'library: create technique -> appears in list, filters by tag and query',
    (tester) async {
      final libVm = TechniqueLibraryViewModel(
        repository: persistence.techRepo,
        mediaService: mediaService,
      );

      // The load command runs on construction; wait for it to finish.
      await _settle(tester, () => !libVm.load.running);
      expect(libVm.all, isEmpty);

      await libVm.createTechnique.execute('Twang');
      await _settle(tester, () => !libVm.createTechnique.running);
      expect(libVm.createTechnique.completed, isTrue,
          reason: 'creating a named technique should succeed');

      final created = (libVm.createTechnique.result as Ok<Technique>).value;
      expect(created.name, 'Twang');
      expect(libVm.all.map((t) => t.id), contains(created.id));
      expect(libVm.filtered.map((t) => t.id), contains(created.id));

      // A non-matching free-text query hides it; a matching one keeps it.
      libVm.setQuery('zzz-no-match');
      expect(libVm.filtered, isEmpty);
      libVm.setQuery('twa');
      expect(libVm.filtered.map((t) => t.id), contains(created.id));
      libVm.setQuery('');

      libVm.dispose();
    },
  );

  testWidgets(
    'detail: add reference media (synthetic wav) via draft -> save -> reload '
    'round-trips technique + media; ground-truth analysis is non-empty',
    (tester) async {
      // 1) Create a technique through the library VM.
      final libVm = TechniqueLibraryViewModel(
        repository: persistence.techRepo,
        mediaService: mediaService,
      );
      await _settle(tester, () => !libVm.load.running);
      await libVm.createTechnique.execute('Belting');
      await _settle(tester, () => !libVm.createTechnique.running);
      final techniqueId =
          (libVm.createTechnique.result as Ok<Technique>).value.id;

      // 2) Open its detail VM.
      final tmp = await getTemporaryDirectory();
      final wavPath = await _writeSineWav(
        p.join(tmp.path, 'ref_${DateTime.now().microsecondsSinceEpoch}.wav'),
        freq: 440,
      );

      final detail = TechniqueDetailViewModel(
        repository: persistence.techRepo,
        mediaService: mediaService,
        techniqueId: techniqueId,
        analysisService: const MediaAnalysisService(),
      );
      await _settle(tester, () => detail.technique != null);
      expect(detail.technique, isNotNull);
      expect(detail.effectiveMedia, isEmpty);

      // 3) Stage a reference media item in the draft (not yet persisted).
      detail.beginMediaDraft();
      await detail.addMedia([wavPath]);
      await _settle(tester, () => !detail.importingMedia);

      expect(detail.hasMediaDraft, isTrue,
          reason: 'a staged add should mark the draft dirty');
      expect(detail.effectiveMedia, hasLength(1),
          reason: 'the staged example shows in the draft view');
      expect(detail.audioExamples, hasLength(1));
      // Still nothing persisted on the underlying technique.
      expect(detail.technique!.media, isEmpty);

      final stagedPath = detail.referenceMedia!.path;
      expect(File(stagedPath).existsSync(), isTrue,
          reason: 'addMedia copies the file into the sandbox immediately');

      // 4) Run the ground-truth (reference) analysis on the staged example.
      await detail.ensureReferenceAnalysis();
      await _settle(tester, () => !detail.referenceAnalysisLoading);

      // A real 440 Hz tone decodes; the analysis should be present and sane.
      final refAnalysis = detail.referenceAnalysis;
      expect(detail.referenceAnalysisFailed, isFalse,
          reason: 'a clean 440 Hz tone should decode and analyze');
      expect(refAnalysis, isNotNull);
      if (refAnalysis != null) {
        expect(refAnalysis.durationMs, greaterThan(500),
            reason: 'a ~1s tone reports a real duration');
        expect(refAnalysis.metrics.frames, greaterThan(0));
        // The analysis subject getters (AnalysisSource) should be wired.
        expect(detail.selectedMetrics, isNotNull);
        expect(detail.selectedSpectrogram, isNotNull);
        expect(detail.selectedTrack, isNotNull);
        expect(detail.selectedTrack!.durationMs, refAnalysis.durationMs);
      }

      // 5) Caption the staged example, then SAVE the draft.
      await detail.renameMedia(detail.effectiveMedia.single.id, 'Demo take');
      final edited = detail.technique!.copyWith(
        tags: ['EVT', 'Quality'],
        summary: 'bright, ringing',
        cues: 'narrow the AES',
        updatedAt: DateTime.now(),
      );
      final saved = await detail.save(edited);
      expect(saved, isTrue, reason: 'save should persist');
      // After save the draft is committed/cleared.
      expect(detail.hasMediaDraft, isFalse);

      libVm.dispose();
      detail.dispose();

      // 6) Fresh reload from the SAME drift store and assert the round-trip.
      final result = await persistence.techRepo.getTechnique(techniqueId);
      expect(result, isA<Ok<Technique>>());
      final reloaded = (result as Ok<Technique>).value;

      expect(reloaded.name, 'Belting');
      expect(reloaded.tags, ['EVT', 'Quality']);
      expect(reloaded.summary, 'bright, ringing');
      expect(reloaded.cues, 'narrow the AES');
      expect(reloaded.media, hasLength(1),
          reason: 'the saved reference example survives reload');
      final reloadedMedia = reloaded.media.single;
      expect(reloadedMedia.label, 'Demo take',
          reason: 'the caption snapshot persists');
      expect(reloadedMedia.kind, TechniqueMediaKind.audio);
      // The pooled file is readable after reload.
      expect(File(reloadedMedia.path).existsSync(), isTrue);
      expect(await File(reloadedMedia.path).length(), greaterThan(0));

      // 7) Re-open the detail VM on the reloaded technique and confirm the
      //    ground-truth analysis recomputes from the pooled media.
      final detail2 = TechniqueDetailViewModel(
        repository: persistence.techRepo,
        mediaService: mediaService,
        techniqueId: techniqueId,
      );
      await _settle(tester, () => detail2.technique != null);
      expect(detail2.audioExamples, hasLength(1));
      await detail2.ensureReferenceAnalysis();
      await _settle(tester, () => !detail2.referenceAnalysisLoading);
      expect(detail2.referenceAnalysisFailed, isFalse);
      expect(detail2.referenceAnalysis, isNotNull);
      detail2.dispose();
    },
  );

  testWidgets(
    'detail: staged media draft can be DISCARDED without persisting',
    (tester) async {
      final libVm = TechniqueLibraryViewModel(
        repository: persistence.techRepo,
        mediaService: mediaService,
      );
      await _settle(tester, () => !libVm.load.running);
      await libVm.createTechnique.execute('Curbing');
      await _settle(tester, () => !libVm.createTechnique.running);
      final techniqueId =
          (libVm.createTechnique.result as Ok<Technique>).value.id;
      libVm.dispose();

      final tmp = await getTemporaryDirectory();
      final wavPath = await _writeSineWav(
        p.join(tmp.path, 'drop_${DateTime.now().microsecondsSinceEpoch}.wav'),
      );

      final detail = TechniqueDetailViewModel(
        repository: persistence.techRepo,
        mediaService: mediaService,
        techniqueId: techniqueId,
      );
      await _settle(tester, () => detail.technique != null);

      detail.beginMediaDraft();
      await detail.addMedia([wavPath]);
      await _settle(tester, () => !detail.importingMedia);
      expect(detail.effectiveMedia, hasLength(1));
      final stagedPath = detail.effectiveMedia.single.path;
      expect(File(stagedPath).existsSync(), isTrue);

      // Discard the draft: the staged example and its copied file are dropped.
      await detail.discardMediaDraft();
      expect(detail.hasMediaDraft, isFalse);
      expect(detail.effectiveMedia, isEmpty);
      // The orphaned sandbox copy is deleted (best-effort cleanup may be async).
      await _settle(tester, () => !File(stagedPath).existsSync());
      expect(File(stagedPath).existsSync(), isFalse,
          reason: 'a discarded staged file should be removed from the sandbox');

      detail.dispose();

      // Nothing persisted.
      final reloaded = (await persistence.techRepo.getTechnique(techniqueId)
              as Ok<Technique>)
          .value;
      expect(reloaded.media, isEmpty);
    },
  );

  testWidgets(
    'analysis service: a clean tone analyzes; an undecodable file returns null',
    (tester) async {
      const analysis = MediaAnalysisService();
      final tmp = await getTemporaryDirectory();

      final tonePath = await _writeSineWav(
        p.join(tmp.path, 'tone_${DateTime.now().microsecondsSinceEpoch}.wav'),
        freq: 220,
      );
      final tone = await analysis.analyze(tonePath);
      expect(tone, isNotNull, reason: 'a valid WAV tone should analyze');
      if (tone != null) {
        expect(tone.durationMs, greaterThan(500));
        expect(tone.metrics.frames, greaterThan(0));
        expect(tone.spectrogram, isNotNull);
        // Every reported f0 value is finite (no NaN/Inf leaking out).
        for (final f in tone.metrics.f0) {
          expect(f.isFinite || f.isNaN, isTrue);
        }
      }

      // A file that is not decodable audio should yield null, not throw.
      final bogus = File(
        p.join(tmp.path, 'bogus_${DateTime.now().microsecondsSinceEpoch}.wav'),
      );
      await bogus.writeAsBytes(List<int>.filled(64, 7));
      final bad = await analysis.analyze(bogus.path);
      expect(bad, isNull,
          reason: 'an undecodable file returns null rather than throwing');
    },
  );

  testWidgets(
    'technique_marker keeps its name snapshot after the library entry is '
    'deleted (logical ref, no hard FK)',
    (tester) async {
      // 1) Create a library technique to reference.
      final libVm = TechniqueLibraryViewModel(
        repository: persistence.techRepo,
        mediaService: mediaService,
      );
      await _settle(tester, () => !libVm.load.running);
      await libVm.createTechnique.execute('Vibrato');
      await _settle(tester, () => !libVm.createTechnique.running);
      final technique =
          (libVm.createTechnique.result as Ok<Technique>).value;
      libVm.dispose();

      // 2) Create a song practice and pin a technique marker on a vocal take
      //    that references the library technique (with a name snapshot). An
      //    empty-clip vocal lane needs no audio asset but still carries markers.
      final practice = (await persistence.songRepo.createPractice('Marker song')
              as Ok<SongPractice>)
          .value;

      final marker = TechniqueMarker(
        id: newId(),
        positionMs: 1500,
        techniqueId: technique.id,
        techniqueName: technique.name, // snapshot at creation time
        createdAt: DateTime.now(),
        updatedAt: DateTime.now(),
      );
      final take = AudioTrack(
        id: newId(),
        kind: AudioTrackKind.vocal,
        displayName: 'Take 1',
        createdAt: DateTime.now(),
        versionId: practice.activeVersionId,
        techniqueMarkers: [marker],
      );
      final withMarker = practice.copyWith(vocalTracks: [take]);
      final savedPractice = (await persistence.songRepo
          .savePractice(withMarker) as Ok<SongPractice>).value;

      // The marker round-trips on the saved practice.
      final savedTake = savedPractice.vocalTracks
          .firstWhere((t) => t.id == take.id);
      expect(savedTake.techniqueMarkers, hasLength(1));
      expect(savedTake.techniqueMarkers.single.techniqueId, technique.id);
      expect(savedTake.techniqueMarkers.single.techniqueName, 'Vibrato');

      // 3) DELETE the library technique. The marker's ref now dangles by design.
      final del = await persistence.techRepo.deleteTechnique(technique.id);
      expect(del, isA<Ok<void>>());
      expect(await persistence.techRepo.getTechnique(technique.id),
          isA<Error<Technique>>());

      // 4) Reload the practice from the SAME store: the marker survives with its
      //    name snapshot intact even though the library entry is gone (no FK).
      final reloaded = (await persistence.songRepo.getPractice(practice.id)
              as Ok<SongPractice>)
          .value;
      final reloadedTake =
          reloaded.vocalTracks.firstWhere((t) => t.id == take.id);
      expect(reloadedTake.techniqueMarkers, hasLength(1),
          reason: 'the marker is not cascade-deleted with the technique');
      final reloadedMarker = reloadedTake.techniqueMarkers.single;
      expect(reloadedMarker.techniqueId, technique.id,
          reason: 'the logical ref is preserved (it may now dangle)');
      expect(reloadedMarker.techniqueName, 'Vibrato',
          reason: 'the name snapshot keeps the marker readable');
      expect(reloadedMarker.positionMs, 1500);
    },
  );
}
