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
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/ui/song_practice_detail/view_model/song_practice_detail_view_model.dart';
import 'package:vocara/utils/result.dart';

/// Writes a mono 16-bit PCM WAV sine tone and returns its path.
Future<String> _writeSineWav(String path, {int seconds = 1}) async {
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
    final value = (sin(2 * pi * 440 * i / sampleRate) * 0.5 * 32767).round();
    pcm.setInt16(i * 2, value, Endian.little);
  }
  bytes.add(pcm.buffer.asUint8List());

  final file = File(path);
  await file.parent.create(recursive: true);
  await file.writeAsBytes(bytes.toBytes());
  return path;
}

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  MediaKit.ensureInitialized();

  testWidgets('import → waveform analysis → multi-track playback pipeline', (
    tester,
  ) async {
    final tmp = await getTemporaryDirectory();
    final wavPath = await _writeSineWav(
      p.join(tmp.path, 'tone_${DateTime.now().microsecondsSinceEpoch}.wav'),
    );

    final storage = LocalStorageService();
    final persistence = Persistence.memory();
    final repo = persistence.songRepo;

    final created = await repo.createPractice('Pipeline test');
    expect(created, isA<Ok<SongPractice>>());
    final practiceId = (created as Ok<SongPractice>).value.id;

    // Fire-and-forget load, matching the production wiring in
    // dependencies.dart (the view model tolerates attach-before-load).
    final appSettings = AppSettingsService(storage: storage);
    unawaited(appSettings.load());

    final vm = SongPracticeDetailViewModel(
      repository: repo,
      techniqueRepository: persistence.techRepo,
      audioFileService: AudioFileService(storage: storage),
      waveformService: WaveformService(storage: storage),
      spectrogramService: const SpectrogramService(),
      separationService: FakeSourceSeparationService(),
      appSettings: appSettings,
      practiceId: practiceId,
    );

    // Let the initial load command settle.
    for (var i = 0; i < 20 && vm.practice == null; i++) {
      await tester.pump(const Duration(milliseconds: 100));
    }
    expect(vm.practice, isNotNull);

    // Import the generated tone as the accompaniment.
    await vm.addAccompaniment(wavPath);
    for (var i = 0; i < 50 && vm.busy; i++) {
      await tester.pump(const Duration(milliseconds: 100));
    }

    final accompaniment = vm.practice!.accompaniment;
    expect(accompaniment, isNotNull, reason: 'accompaniment should be set');
    expect(
      accompaniment!.primaryClip!.waveformPeaks,
      isNotEmpty,
      reason: 'just_waveform should produce peaks',
    );
    expect(
      accompaniment.durationMs,
      greaterThan(500),
      reason: 'a ~1s tone should report a real duration',
    );
    expect(vm.errorMessage, isNull);

    // Exercise the media_kit transport: play, then pause.
    await vm.togglePlay();
    await tester.pump(const Duration(milliseconds: 400));
    await vm.togglePlay();
    await tester.pump(const Duration(milliseconds: 100));

    // Pin a note at the playhead on the accompaniment track.
    await vm.addNoteAtPlayhead(accompaniment.id, 'test note');
    await tester.pump(const Duration(milliseconds: 100));
    expect(vm.practice!.accompaniment!.notes, hasLength(1));

    vm.dispose();
    await persistence.db.close();
  });
}
