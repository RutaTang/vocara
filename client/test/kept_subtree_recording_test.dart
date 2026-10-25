// The detail screen keeps unchanged subtrees across rebuilds. This walk takes
// it through a recording — the armed lane, the live take growing tick by
// tick, edits made while it runs — and the take edits after, checking each
// step's kept screen against a fresh build (see FreshBuildOracle).

import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:provider/provider.dart';
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
import 'package:vocara/domain/models/spectrogram.dart';
import 'package:vocara/domain/models/take_version.dart';
import 'package:vocara/domain/models/voice_metrics.dart';
import 'package:vocara/ui/song_practice_detail/view_model/song_practice_detail_view_model.dart';
import 'package:vocara/ui/song_practice_detail/widgets/song_practice_detail_screen.dart';
import 'package:vocara/utils/result.dart';

import 'support/fresh_build_oracle.dart';

/// In-memory repository: [stored] plays the on-disk aggregate.
class _FakeRepo extends Fake implements SongPracticeRepository {
  _FakeRepo(this.stored);

  SongPractice stored;

  @override
  Future<Result<Loaded<SongPractice>>> loadPractice(String id) async =>
      Result.ok(Loaded(stored, const {}));

  @override
  Future<Result<Loaded<SongPractice>>> savePracticeWithBase(
    SongPractice practice, {
    required Map<String, String> baseHashes,
  }) async {
    await Future<void>.delayed(Duration.zero);
    stored = practice;
    return Result.ok(Loaded(stored, const {}));
  }
}

class _FakeTechRepo extends Fake implements TechniqueRepository {}

/// Analyzes nothing.
class _FakeSpectrograms extends Fake implements SpectrogramService {
  @override
  Future<Spectrogram?> computeFromFile(
    String path, {
    int fftSize = 2048,
    int hop = 512,
    double maxHz = 5000,
  }) async => null;

  @override
  Future<VoiceMetrics?> computeMetricsFromFile(
    String path, {
    int fftSize = 2048,
    int hop = 512,
  }) async => null;

  @override
  Future<TakeAnalysisBundle?> computeBothFromFile(
    String path, {
    int fftSize = 2048,
    int hop = 512,
    double maxHz = 5000,
  }) async => null;
}

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

class _FakeWaveforms extends Fake implements WaveformService {
  @override
  Future<WaveformResult> analyze(
    String audioFilePath, {
    int targetBuckets = 1000,
  }) async => (peaks: const <double>[], durationMs: 1000);
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
  Future<String> concatWav({
    required String practiceId,
    required String firstPath,
    required String secondPath,
  }) async => '/edit/merged.wav';

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
  Duration get currentPosition => Duration.zero;
  @override
  bool get isPlaying => false;
  @override
  void setTrackPan(String id, double pan) {}
  @override
  void setTrackFx(String trackId, TrackFx fx) {}
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

/// The native engine as far as recording and the settings attach touch it.
class _FakeGraphEngine extends _FakeEngine
    implements GraphAudioPlaybackService {
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
  int inputStreamState() => 0; // closed until a take opens it

  @override
  void setInputChannel(int mode) {}
  @override
  void setMonitorGain(double gain) {}
  @override
  void setEchoFree(String uid) {}
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
/// setup) and arms.
class _FakeMic extends Fake implements MicRecordingController {
  bool _recording = false;
  bool _starting = false;
  String? _path;

  @override
  final ValueNotifier<double> level = ValueNotifier(0);
  @override
  bool get isRecording => _recording;
  @override
  bool get isStarting => _starting;

  /// Set by [invalidatePendingStart] (a Stop in the arm window).
  bool _invalidated = false;

  @override
  Future<MicRecordingStart> start(RecordingPathStrategy destination) async {
    _starting = true;
    _invalidated = false;
    try {
      _path = await destination.create();
      if (_invalidated) return const MicRecordingStart.invalidated();
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
  void invalidatePendingStart() => _invalidated = true;
  @override
  void dispose() {}
}

SongPractice _recSeed() {
  List<double> peaks(int seed) {
    final r = math.Random(seed);
    return [for (var i = 0; i < 300; i++) r.nextDouble()];
  }

  return SongPractice(
    id: 'p1',
    title: 'Song',
    createdAt: DateTime(2026),
    updatedAt: DateTime(2026),
    timelineDurationMs: 60000,
    versions: [
      TakeVersion(
        id: 'v1',
        name: 'Interpretation 1',
        createdAt: DateTime(2026),
      ),
    ],
    activeVersionId: 'v1',
    accompaniment: AudioTrack(
      id: 'acc',
      kind: AudioTrackKind.accompaniment,
      displayName: 'Backing',
      audioFilePath: '/pool/acc.wav',
      durationMs: 60000,
      waveformPeaks: peaks(0),
      createdAt: DateTime(2026),
    ),
    vocalTracks: [
      for (var i = 1; i <= 3; i++)
        AudioTrack(
          id: 'take$i',
          kind: AudioTrackKind.vocal,
          displayName: 'Take $i',
          audioFilePath: '/pool/take$i.wav',
          durationMs: 20000,
          startMs: 0,
          waveformPeaks: peaks(i),
          createdAt: DateTime(2026),
          versionId: 'v1',
        ),
    ],
  );
}

void main() {
  testWidgets('recording: kept subtrees equal a fresh build', (tester) async {
    final oracle = FreshBuildOracle(tester);
    tester.view.physicalSize = const Size(1800, 1100);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    final repo = _FakeRepo(_recSeed());
    final mic = _FakeMic();
    final vm = SongPracticeDetailViewModel(
      repository: repo,
      techniqueRepository: _FakeTechRepo(),
      audioFileService: _FakeFiles(),
      waveformService: _FakeWaveforms(),
      spectrogramService: _FakeSpectrograms(),
      separationService: FakeSourceSeparationService(available: false),
      appSettings: AppSettingsService(
        storage: LocalStorageService(),
        enumerateOutputs: () => const [],
        enumerateInputs: () => const [],
      ),
      practiceId: 'p1',
      playbackEngine: _FakeGraphEngine(),
      editService: _FakeEdits(),
      mic: mic,
    );
    for (var i = 0; i < 100 && vm.practice == null; i++) {
      await tester.pump(const Duration(milliseconds: 5));
    }
    expect(vm.practice, isNotNull);
    vm.toggleLyricsPanel();
    await tester.pumpWidget(
      oracle.wrap(
        MaterialApp(
          home: ChangeNotifierProvider.value(
            value: vm,
            child: const SongPracticeDetailScreen(),
          ),
        ),
      ),
    );
    await tester.pump(const Duration(milliseconds: 50));
    await oracle.expectFresh('baseline');
    vm.selectTrack('take2');
    await oracle.expectFresh('arm take2');
    final start = vm.recording.startRecording();
    for (var i = 0; i < 20; i++) {
      await tester.pump(const Duration(milliseconds: 10));
    }
    await start;
    expect(vm.isRecording, isTrue);
    await oracle.expectFresh('recording started');
    final peaks = <double>[];
    for (var k = 0; k < 3; k++) {
      for (var j = 0; j < 40; j++) {
        peaks.add(0.2 + 0.6 * ((j * 7 + k) % 10) / 10);
      }
      vm.recording.recordingPeaks.value = List.unmodifiable(peaks);
      vm.position.value = Duration(
        milliseconds: vm.recording.recordingStartMs + peaks.length * 50,
      );
      await oracle.expectFresh('recording tick $k');
    }
    vm.mixer.setTrackVolumeLive('take1', 0.3);
    await oracle.expectFresh('fader during take');
    unawaited(vm.mixer.toggleSolo('take3'));
    await oracle.expectFresh('solo during take');
    vm.selectTrack('take3');
    await oracle.expectFresh('select other during take');
    final stop = vm.stopRecording();
    for (var i = 0; i < 40; i++) {
      await tester.pump(const Duration(milliseconds: 10));
    }
    await stop;
    expect(vm.isRecording, isFalse);
    await oracle.expectFresh('recording stopped');
    vm.selectTrack('take3');
    final start2 = vm.recording.startRecording();
    for (var i = 0; i < 20; i++) {
      await tester.pump(const Duration(milliseconds: 10));
    }
    await start2;
    await oracle.expectFresh('second take started');
    final stop2 = vm.stopRecording();
    for (var i = 0; i < 40; i++) {
      await tester.pump(const Duration(milliseconds: 10));
    }
    await stop2;
    await oracle.expectFresh('second take stopped');

    // A take edit runs to its end (it saves, then analyzes) before the check.
    Future<void> step(String what, FutureOr<void> Function() change) async {
      final r = change();
      if (r is Future) {
        var done = false;
        unawaited(
          r
              .catchError((Object e) => oracle.failures.add('$what threw: $e'))
              .whenComplete(() => done = true),
        );
        for (var i = 0; i < 60 && !done; i++) {
          await tester.pump(const Duration(milliseconds: 10));
        }
      }
      await oracle.expectFresh(what);
    }

    vm.position.value = const Duration(milliseconds: 5000);
    await step('dup take1', () => vm.duplicateTrack('take1'));
    await step('split take1', () => vm.splitTrackAtPlayhead('take1'));
    await step('merge take1 down', () => vm.mergeTrackDown('take1'));
    final t2 = vm.practice!.vocalTracks.firstWhere((t) => t.id == 'take2');
    await step('delete clip', () => vm.deleteClip('take2', t2.clips.last.id));
    await step('import take', () => vm.addVocalTake('/src/new.wav'));
    await step(
      'add clip to take3',
      () => vm.addClipToTake('take3', '/src/c.wav'),
    );
    await step('reanalyze take3', () => vm.reanalyzeTrack('take3'));
    await step('note on take3', () => vm.addNote('take3', 4000, null, 'n'));
    final note = vm.practice!.vocalTracks
        .firstWhere((t) => t.id == 'take3')
        .notes
        .first;
    await step(
      'note moved',
      () => vm.updateNote('take3', note.copyWith(positionMs: 9000)),
    );
    await step('note deleted', () => vm.deleteNote('take3', note.id));
    await step(
      'technique on take3',
      () => vm.addTechnique('take3', 'tid', 'Belt', 1000, 3000),
    );
    await step('color reset', () => vm.setTrackColor('take3', null));
    await step('text scale', () {
      tester.platformDispatcher.textScaleFactorTestValue = 1.4;
    });
    await step('text scale back', () {
      tester.platformDispatcher.clearTextScaleFactorTestValue();
    });
    await step('delete take', () => vm.deleteTrack('take3'));
    // Disposed inside the test: its pending save and analysis timers must be
    // gone before the binding checks for timers left running.
    await tester.pumpWidget(const SizedBox());
    vm.dispose();
    expect(oracle.failures, isEmpty);
  });
}
