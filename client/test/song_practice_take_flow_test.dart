import 'dart:async';

import 'package:flutter/foundation.dart' show ValueNotifier;
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/repositories/song_practice_repository.dart';
import 'package:vocara/data/repositories/technique_repository.dart';
import 'package:vocara/data/services/app_settings_service.dart';
import 'package:vocara/data/services/audio_edit_service.dart';
import 'package:vocara/data/services/audio_file_service.dart';
import 'package:vocara/data/services/graph_audio_playback_service.dart';
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/data/services/mic_recording_controller.dart';
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
  int recordings = 0;

  @override
  Future<String> newRecordingPath({
    required String practiceId,
    String extension = 'wav',
  }) async => '/audio/p1/rec${recordings++}.wav';

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

  @override
  Future<({String firstPath, String secondPath})> splitWav({
    required String practiceId,
    required String sourcePath,
    required int splitFileMs,
  }) async => (firstPath: '/edit/first.wav', secondPath: '/edit/second.wav');

  @override
  Future<String> duplicate({
    required String practiceId,
    required String sourcePath,
  }) async => '/edit/dup.wav';
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

/// The native engine as far as recording and the settings attach touch it,
/// with every source's last pushed mute remembered.
class _FakeGraphEngine extends _FakeEngine
    implements GraphAudioPlaybackService {
  final Map<String, bool> muted = {};

  @override
  Future<void> sync(List<TrackPlayback> tracks, List<BusPlayback> buses) async {
    for (final t in tracks) {
      muted[t.id] = t.muted;
    }
  }

  @override
  Future<void> setVolume(
    String id,
    double volume, {
    bool muted = false,
  }) async => this.muted[id] = muted;

  @override
  bool get isAvailable => true;
  @override
  bool setOutputDevice(String uid) => true;
  @override
  bool setInputDevice(String uid) => true;
  @override
  void setMonitorReverb(double room, double damping, double wet) {}
  @override
  void setInputGain(double gain) {}
  @override
  void setMonitorGain(double gain) {}
  @override
  String actualOutputDevice() => '';
  @override
  String currentOutputDevice() => '';
  @override
  int pollDeviceEvents(int mask) => 0;
  @override
  int streamXruns() => 0;
  @override
  int inputFramesReceived() => 0;
  @override
  int recordWrittenFrames() => 0;
  @override
  double recordLatencyMs() => 0;
}

/// Recorder double: [start] runs the destination's create() (the VM's take
/// setup) and, when [ready] is set, parks there — the mic's ready handshake,
/// up to two seconds on a cold open — before arming.
class _FakeMic extends Fake implements MicRecordingController {
  bool _recording = false;
  bool _starting = false;
  String? _path;
  Completer<void>? ready;

  @override
  final ValueNotifier<double> level = ValueNotifier(0);
  @override
  bool get isRecording => _recording;
  @override
  bool get isStarting => _starting;

  @override
  Future<MicRecordingStart> start(RecordingPathStrategy destination) async {
    _starting = true;
    try {
      _path = await destination.create();
      final r = ready;
      if (r != null) await r.future;
      _recording = true;
      return MicRecordingStart.started(_path!);
    } finally {
      _starting = false;
    }
  }

  @override
  MicRecordingStop? stop() {
    if (!_recording) return null;
    _recording = false;
    return (path: _path!, clean: true, droppedFrames: 0);
  }

  @override
  Future<void> guardStopFinalize(Future<void> Function() work) => work();
  @override
  void invalidatePendingStart() {}
  @override
  void dispose() {}
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
    MicRecordingController? mic,
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
      mic: mic,
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

  group('the take window guards every structural edit', () {
    test('Stop → an edit inside the stop window is refused and the take '
        'lands on its lane', () async {
      final mic = _FakeMic();
      final vm = await pumpVm(engine: _FakeGraphEngine(), mic: mic);
      vm.selectTrack('take1'); // arm the existing take
      await vm.recording.startRecording();
      expect(vm.isRecording, isTrue);
      vm.position.value = const Duration(milliseconds: 500);

      final stop = vm.stopRecording();
      // isRecording already reads false and the gate is free, but the take is
      // not in the model yet.
      expect(vm.isRecording, isFalse);
      expect(vm.recording.isStopping, isTrue);
      final edits = [
        vm.deleteTrack('take1'),
        vm.duplicateTrack('take1'),
        vm.splitTrackAtPlayhead('take1'),
        vm.reanalyzeTrack('take1'),
        vm.addVocalTake('/src/drop.wav'),
        vm.addClipToTake('take1', '/src/drop.wav'),
      ];
      expect(vm.busy, isFalse, reason: 'an edit took the gate mid-stop');
      await Future.wait(edits);
      await stop;

      final take1 = repo.stored.vocalTracks.singleWhere((t) => t.id == 'take1');
      expect(take1.clips, hasLength(2), reason: 'the take punched into take1');
      expect(repo.stored.vocalTracks, hasLength(1));
    });

    test('REC → an edit inside the arm window is refused', () async {
      final mic = _FakeMic()..ready = Completer<void>();
      final vm = await pumpVm(engine: _FakeGraphEngine(), mic: mic);
      vm.selectTrack('take1');
      final start = vm.recording.startRecording();
      await Future<void>.delayed(const Duration(milliseconds: 10));
      expect(vm.recording.isStartingTake, isTrue);
      await vm.deleteTrack('take1');
      expect(vm.practice!.vocalTracks.map((t) => t.id), ['take1']);
      mic.ready!.complete();
      await start;
      expect(vm.recording.recordTargetId, 'take1');
      await vm.stopRecording();
      expect(
        repo.stored.vocalTracks.singleWhere((t) => t.id == 'take1').clips,
        hasLength(2),
      );
    });
  });
}
