import 'dart:async';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/repositories/song_practice_repository.dart';
import 'package:vocara/data/repositories/technique_repository.dart';
import 'package:vocara/data/services/app_settings_service.dart';
import 'package:vocara/data/services/audio_edit_service.dart';
import 'package:vocara/data/services/audio_file_service.dart';
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/data/services/playback_engine.dart';
import 'package:vocara/data/services/source_separation_service.dart';
import 'package:vocara/data/services/spectrogram_service.dart';
import 'package:vocara/data/services/waveform_service.dart';
import 'package:vocara/data/sync/sync_hashes.dart';
import 'package:vocara/domain/models/audio_fx.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/domain/models/take_version.dart';
import 'package:vocara/ui/song_practice_detail/view_model/song_practice_detail_view_model.dart';
import 'package:vocara/utils/result.dart';

// Flows that cross the detail screen's components: a take travelling from the
// record/import entry points through the model, the save and the analysis
// caches, with the user acting in the windows between their awaits.

/// In-memory repository: [stored] plays the on-disk aggregate.
class _FakeRepo extends Fake implements SongPracticeRepository {
  _FakeRepo(this.stored);

  SongPractice stored;

  /// Fails only the NEXT save, then heals.
  bool failNextSave = false;

  @override
  Future<Result<Loaded<SongPractice>>> loadPractice(String id) async =>
      Result.ok(Loaded(stored, const {}));

  @override
  Future<Result<Loaded<SongPractice>>> savePracticeWithBase(
    SongPractice practice, {
    required Map<String, String> baseHashes,
  }) async {
    await Future<void>.delayed(Duration.zero);
    if (failNextSave) {
      failNextSave = false;
      return Result.error(Exception('save failed'));
    }
    stored = practice;
    return Result.ok(Loaded(stored, const {}));
  }
}

class _FakeTechRepo extends Fake implements TechniqueRepository {}

class _FakeFiles extends Fake implements AudioFileService {
  int imports = 0;

  @override
  Future<String> importAudio({
    required String practiceId,
    required String sourcePath,
  }) async => '/audio/p1/import${imports++}.wav';

  @override
  Future<void> deleteAudio(String path) async {}
}

/// Waveform analysis that parks until [gate] completes when one is set — the
/// seconds a real analyze takes, during which the UI stays interactive.
class _FakeWaveforms extends Fake implements WaveformService {
  Completer<void>? gate;

  @override
  Future<WaveformResult> analyze(
    String audioFilePath, {
    int targetBuckets = 1000,
  }) async {
    final g = gate;
    if (g != null) await g.future;
    return (peaks: const <double>[], durationMs: 1000);
  }
}

class _FakeEdits extends Fake implements AudioEditService {
  @override
  Future<String> ensureStreamableWav({
    required String practiceId,
    required String path,
  }) async => path;
}

class _FakeEngine extends Fake implements PlaybackEngine {
  @override
  Stream<Duration> get position => const Stream.empty();
  @override
  Stream<bool> get playing => const Stream.empty();
  @override
  bool get isPlaying => false;
  @override
  bool get supportsEffects => false;
  @override
  void setLoop(bool enabled, int startMs, int endMs) {}
  @override
  Future<void> sync(
    List<TrackPlayback> tracks,
    List<BusPlayback> buses,
  ) async {}
  @override
  Future<void> play() async {}
  @override
  Future<void> pause() async {}
  @override
  Future<void> seek(Duration position) async {}
  @override
  Future<void> setVolume(
    String id,
    double volume, {
    bool muted = false,
  }) async {}
  @override
  void setBusVolume(String busId, double volume, {bool muted = false}) {}
  @override
  void setMasterVolume(double gain) {}
  @override
  void setReverb(ReverbSettings reverb) {}
  @override
  Future<void> dispose() async {}
}

SongPractice _seed({
  String? activeVersionId = 'v1',
  AudioTrack? accompaniment,
  List<AudioTrack>? vocalTracks,
}) => SongPractice(
  id: 'p1',
  title: 'Song',
  createdAt: DateTime(2026),
  updatedAt: DateTime(2026),
  versions: [
    TakeVersion(id: 'v1', name: 'Interpretation 1', createdAt: DateTime(2026)),
    TakeVersion(id: 'v2', name: 'Interpretation 2', createdAt: DateTime(2026)),
  ],
  activeVersionId: activeVersionId,
  accompaniment: accompaniment,
  vocalTracks:
      vocalTracks ??
      [
        AudioTrack(
          id: 'take1',
          kind: AudioTrackKind.vocal,
          displayName: 'Take 1',
          audioFilePath: '/pool/take1.wav',
          durationMs: 1000,
          createdAt: DateTime(2026),
          versionId: 'v1',
        ),
      ],
);

void main() {
  late _FakeRepo repo;
  late _FakeWaveforms waveforms;

  Future<SongPracticeDetailViewModel> pumpVm({
    SongPractice? seed,
    PlaybackEngine? engine,
  }) async {
    repo = _FakeRepo(seed ?? _seed());
    waveforms = _FakeWaveforms();
    final vm = SongPracticeDetailViewModel(
      repository: repo,
      techniqueRepository: _FakeTechRepo(),
      audioFileService: _FakeFiles(),
      waveformService: waveforms,
      spectrogramService: const SpectrogramService(),
      separationService: FakeSourceSeparationService(available: false),
      appSettings: AppSettingsService(
        storage: LocalStorageService(),
        enumerateOutputs: () => const [],
        enumerateInputs: () => const [],
      ),
      practiceId: 'p1',
      playbackEngine: engine ?? _FakeEngine(),
      editService: _FakeEdits(),
    );
    addTearDown(vm.dispose);
    for (var i = 0; i < 400 && vm.practice == null; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }
    expect(vm.practice, isNotNull, reason: 'initial load should complete');
    return vm;
  }

  group('an active interpretation id that does not resolve', () {
    // What the sync integrity repair leaves when the active interpretation
    // was deleted on another device: the column is nulled, and the lanes fall
    // back to showing the first interpretation.
    Future<SongPracticeDetailViewModel> pumpUnresolved() =>
        pumpVm(seed: _seed(activeVersionId: null));

    test('still shows the first interpretation and its takes', () async {
      final vm = await pumpUnresolved();
      expect(vm.versions.active?.id, 'v1');
      expect(vm.versions.vocalTakes.map((t) => t.id), ['take1']);
    });

    test('a selected, visible take can be armed', () async {
      final vm = await pumpUnresolved();
      vm.selectTrack('take1');
      expect(vm.recording.recordTargetId, 'take1');
    });

    test("REC's empty take lands in the interpretation on screen", () async {
      final vm = await pumpUnresolved();
      await vm.createEmptyVocalTake();
      final created = vm.practice!.vocalTracks.last;
      expect(created.id, isNot('take1'));
      expect(
        vm.versions.vocalTakes.map((t) => t.id),
        contains(created.id),
        reason: 'a take filed under the raw (null) id is shown nowhere',
      );
    });

    test('an imported take lands in the interpretation on screen', () async {
      final vm = await pumpUnresolved();
      await vm.addVocalTake('/src/song.wav');
      final added = vm.practice!.vocalTracks.last;
      expect(added.clips.single.audioFilePath, '/audio/p1/import0.wav');
      expect(vm.versions.vocalTakes.map((t) => t.id), contains(added.id));
    });
  });
}
