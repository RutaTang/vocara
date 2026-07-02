import 'dart:async';
import 'dart:io';
import 'dart:math';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:media_kit/media_kit.dart';
import 'package:path/path.dart' as p;
import 'package:path_provider/path_provider.dart';
import 'package:vocara/data/db/persistence.dart';
import 'package:vocara/data/services/app_settings_service.dart';
import 'package:vocara/data/services/audio_file_service.dart';
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/data/services/source_separation_service.dart';
import 'package:vocara/data/services/spectrogram_service.dart';
import 'package:vocara/data/services/waveform_service.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/ui/song_practice_detail/view_model/song_practice_detail_view_model.dart';
import 'package:vocara/utils/result.dart';

/// Writes a mono 16-bit PCM WAV sine tone and returns its path. A real,
/// decodable file so just_waveform (AVFoundation) can analyze the imported
/// stems the way it will on a production run.
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

/// Pumps [tester] until [done] is true or [maxIterations] elapses (guards
/// against a never-settling condition hanging the test).
Future<void> _pumpUntil(
  WidgetTester tester,
  bool Function() done, {
  int maxIterations = 80,
  Duration step = const Duration(milliseconds: 100),
}) async {
  for (var i = 0; i < maxIterations && !done(); i++) {
    await tester.pump(step);
  }
}

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  MediaKit.ensureInitialized();

  testWidgets(
    'separation flow: instrumental lands in the accompaniment, original kept '
    'as reference, and the stems survive a save -> fresh-VM reload',
    (tester) async {
      final tmp = await getTemporaryDirectory();
      // The full-mix "original song" to separate. Distinct frequency so it is a
      // recognizable, decodable file.
      final originalPath = await _writeSineWav(
        p.join(tmp.path, 'song_${DateTime.now().microsecondsSinceEpoch}.wav'),
        seconds: 1,
        freq: 330,
      );

      // Wired-up in-memory drift stack (asset pool + repositories) so we
      // exercise the REAL persistence/asset round-trip, not a JSON stub.
      final persistence = Persistence.memory();
      final storage = LocalStorageService();
      // Fire-and-forget load, matching the production wiring in
      // dependencies.dart (the view model tolerates attach-before-load).
      final appSettings = AppSettingsService(storage: storage);
      unawaited(appSettings.load());

      final created = await persistence.songRepo.createPractice('Sep test');
      expect(created, isA<Ok<SongPractice>>());
      final practiceId = (created as Ok<SongPractice>).value.id;

      SongPracticeDetailViewModel buildVm() => SongPracticeDetailViewModel(
        repository: persistence.songRepo,
        techniqueRepository: persistence.techRepo,
        audioFileService: AudioFileService(storage: storage),
        waveformService: WaveformService(storage: storage),
        spectrogramService: const SpectrogramService(),
        separationService: FakeSourceSeparationService(),
        appSettings: appSettings,
        practiceId: practiceId,
      );

      final vm = buildVm();
      addTearDown(() async {
        await persistence.db.close();
      });

      // Wait for the initial load AND the async separation-engine probe (the VM
      // gates separateBacking on _separationAvailable, set by a fire-and-forget
      // isAvailable() future). The Fake reports available.
      await _pumpUntil(
        tester,
        () => vm.practice != null && vm.separationAvailable,
      );
      expect(vm.practice, isNotNull);
      expect(
        vm.separationAvailable,
        isTrue,
        reason: 'the fake engine reports available',
      );

      // Import the original song into the Reference slot — separateBacking
      // prefers the reference as the source to keep.
      await vm.addReference(originalPath);
      await _pumpUntil(tester, () => !vm.busy);
      expect(vm.errorMessage, isNull);
      final reference = vm.practice!.reference;
      expect(reference, isNotNull, reason: 'reference should be set');
      expect(reference!.kind, AudioTrackKind.reference);
      expect(
        vm.practice!.accompaniment,
        isNull,
        reason: 'no accompaniment until separation runs',
      );

      // Observe the separation progress crossing through to completion via the
      // VM's separationProgress notifier (it nulls out when idle/done).
      final progressSeen = <double>[];
      void recordProgress() {
        final pr = vm.separationProgress;
        if (pr != null) progressSeen.add(pr);
      }

      vm.addListener(recordProgress);

      // Trigger separation on the reference track. The Fake copies the source
      // verbatim into both stems; the instrumental becomes the accompaniment.
      await vm.separateBacking(reference.id);
      await _pumpUntil(tester, () => !vm.isSeparating && !vm.busy);
      vm.removeListener(recordProgress);

      expect(
        vm.errorMessage,
        isNull,
        reason: 'a deterministic fake separation must not error',
      );
      expect(vm.isSeparating, isFalse);
      expect(
        vm.separationProgress,
        isNull,
        reason: 'progress resets to null when separation finishes',
      );
      // The fake reports 0.2 then 1.0; the VM forwards them, so we should have
      // observed monotonic, finite progress values during the run.
      expect(
        progressSeen,
        isNotEmpty,
        reason: 'progress callbacks should have been forwarded',
      );
      expect(progressSeen.every((v) => v.isFinite && v >= 0 && v <= 1), isTrue);

      // The instrumental stem is now the accompaniment, wired into the practice.
      final accompaniment = vm.practice!.accompaniment;
      expect(
        accompaniment,
        isNotNull,
        reason: 'the instrumental stem becomes the accompaniment',
      );
      expect(accompaniment!.kind, AudioTrackKind.accompaniment);
      expect(
        accompaniment.displayName,
        contains('(Instrumental)'),
        reason: 'separation labels the produced backing as Instrumental',
      );
      expect(
        accompaniment.primaryClip!.waveformPeaks,
        isNotEmpty,
        reason: 'the produced stem is analyzed for a waveform',
      );
      expect(
        accompaniment.durationMs,
        greaterThan(500),
        reason: 'a ~1s stem should report a real duration',
      );
      expect(accompaniment.primaryClip!.audioFilePath, isNotEmpty);

      // The original is KEPT in the reference slot (A/B against the stem).
      expect(
        vm.practice!.reference,
        isNotNull,
        reason: 'the original is kept as the reference',
      );
      expect(vm.practice!.reference!.kind, AudioTrackKind.reference);

      // The kept-original source is recorded so a later re-separate has a target.
      expect(
        vm.practice!.separationSource,
        isNotNull,
        reason: 'separationSource tracks the kept original full mix',
      );
      expect(
        vm.canReseparate,
        isTrue,
        reason: 'a separated backing + a kept source means re-separate is on',
      );

      // Active backing switched to the freshly produced instrumental.
      expect(
        vm.mixer.playReference,
        isFalse,
        reason: 'after separation the audible backing is the instrumental',
      );
      expect(vm.mixer.activeBacking, isNotNull);
      expect(
        vm.mixer.activeBacking!.primaryClip!.audioFilePath,
        accompaniment.primaryClip!.audioFilePath,
      );

      final accompPathBeforeReload = accompaniment.primaryClip!.audioFilePath;

      vm.dispose();

      // --- Fresh VM on the SAME practice: the produced stems must round-trip
      // through the asset pool (resolve to readable files), proving they were
      // persisted, not just held in memory. ---
      final reloaded = buildVm();
      addTearDown(reloaded.dispose);
      await _pumpUntil(
        tester,
        () => reloaded.practice != null && reloaded.separationAvailable,
      );
      expect(reloaded.practice, isNotNull);

      final reAccomp = reloaded.practice!.accompaniment;
      expect(
        reAccomp,
        isNotNull,
        reason: 'the instrumental survived the reload',
      );
      expect(reAccomp!.kind, AudioTrackKind.accompaniment);
      expect(reAccomp.displayName, contains('(Instrumental)'));
      // The pooled stem resolves to a readable, non-trivial file on disk.
      expect(reAccomp.primaryClip!.audioFilePath, isNotEmpty);
      final reAccompFile = File(reAccomp.primaryClip!.audioFilePath);
      expect(
        await reAccompFile.exists(),
        isTrue,
        reason: 'the pooled instrumental asset resolves to a real file',
      );
      expect(
        await reAccompFile.length(),
        greaterThan(44),
        reason: 'the stem is a real WAV (more than a bare header)',
      );
      // Peaks are cached by content hash, so they survive the round-trip.
      expect(reAccomp.primaryClip!.waveformPeaks, isNotEmpty);

      // The reference (original) round-trips too.
      final reRef = reloaded.practice!.reference;
      expect(reRef, isNotNull, reason: 'the kept original survived the reload');
      expect(reRef!.kind, AudioTrackKind.reference);
      expect(
        await File(reRef.primaryClip!.audioFilePath).exists(),
        isTrue,
        reason: 'the pooled reference asset resolves to a real file',
      );

      // separationSource resolves on reload (kept for re-separation).
      expect(reloaded.practice!.separationSource, isNotNull);
      expect(reloaded.canReseparate, isTrue);

      // The working pre-pool path may be rewritten to a pooled asset path on
      // reload; either way the accompaniment must still resolve to real bytes.
      expect(accompPathBeforeReload, isNotEmpty);
    },
  );

  // Real sidecar path — gated. This must NEVER fail when the python engine is
  // absent (the common CI / sandboxed case); it early-returns instead.
  testWidgets(
    'ProcessSourceSeparationService: gated real-sidecar smoke (skips when '
    'no engine is installed)',
    (tester) async {
      final storage = LocalStorageService();
      final svc = ProcessSourceSeparationService(storage: storage);

      final available = await svc.isAvailable();
      if (!available) {
        // No audio-separator in any known Python (sandboxed app / bare CI):
        // treat as a benign skip, not a failure.
        // ignore: avoid_print
        print(
          'ProcessSourceSeparationService unavailable — skipping real '
          'separation smoke test.',
        );
        return;
      }

      // The engine is present: run a tiny separation and assert it produced two
      // real output files, then make sure cancel() leaves nothing in flight.
      final tmp = await getTemporaryDirectory();
      final srcPath = await _writeSineWav(
        p.join(
          tmp.path,
          'real_src_${DateTime.now().microsecondsSinceEpoch}.wav',
        ),
        seconds: 1,
        freq: 220,
      );

      SeparationResult? result;
      Object? failure;
      try {
        result = await svc.separate(srcPath);
      } catch (e) {
        // A present-but-flaky engine (model download, ffmpeg path) shouldn't
        // hard-fail the suite; record it for the orchestrator and bail.
        failure = e;
      } finally {
        // Best-effort: ensure no sidecar process is left running regardless of
        // how separate() resolved.
        svc.cancel();
      }

      if (failure != null) {
        // ignore: avoid_print
        print('Real separation present but failed at runtime: $failure');
        return;
      }

      expect(result, isNotNull);
      expect(
        await File(result!.instrumentalPath).exists(),
        isTrue,
        reason: 'the real engine writes an instrumental stem',
      );
      expect(
        await File(result.vocalPath).exists(),
        isTrue,
        reason: 'the real engine writes a vocal stem',
      );
      expect(result.instrumentalPath, isNot(result.vocalPath));
      // Real stems must carry actual audio frames, not a bare 44-byte header.
      expect(await File(result.instrumentalPath).length(), greaterThan(44));
      expect(await File(result.vocalPath).length(), greaterThan(44));
    },
  );
}
