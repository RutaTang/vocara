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
import 'package:vocara/domain/models/spectrogram.dart';
import 'package:vocara/domain/models/take_version.dart';
import 'package:vocara/domain/models/voice_metrics.dart';
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

  /// Models the asset pool: a save re-reads every vocal clip at its POOL path,
  /// as SongPracticeRepositoryDrift's hydrate does.
  bool pooling = false;

  /// When set, a save parks until it completes (a slow disk).
  Completer<void>? saveGate;

  static AudioTrack _pooled(AudioTrack t) => t.copyWith(
    clips: [
      for (final c in t.clips)
        c.audioFilePath.startsWith('/pool/')
            ? c
            : c.copyWith(
                audioFilePath: '/pool/${c.audioFilePath.split('/').last}',
              ),
    ],
  );

  @override
  Future<Result<Loaded<SongPractice>>> loadPractice(String id) async =>
      Result.ok(Loaded(stored, const {}));

  @override
  Future<Result<Loaded<SongPractice>>> savePracticeWithBase(
    SongPractice practice, {
    required Map<String, String> baseHashes,
  }) async {
    await Future<void>.delayed(Duration.zero);
    final gate = saveGate;
    if (gate != null) await gate.future;
    if (failNextSave) {
      failNextSave = false;
      return Result.error(Exception('save failed'));
    }
    stored = pooling
        ? practice.copyWith(
            vocalTracks: [for (final t in practice.vocalTracks) _pooled(t)],
          )
        : practice;
    return Result.ok(Loaded(stored, const {}));
  }
}

class _FakeTechRepo extends Fake implements TechniqueRepository {}

/// Records every path an analysis was asked for; analyzes nothing.
class _FakeSpectrograms extends Fake implements SpectrogramService {
  final requested = <String>[];

  /// When set, the paired compute parks until it completes.
  Completer<void>? gate;

  @override
  Future<Spectrogram?> computeFromFile(
    String path, {
    int fftSize = 2048,
    int hop = 512,
    double maxHz = 5000,
  }) async {
    requested.add(path);
    return null;
  }

  @override
  Future<VoiceMetrics?> computeMetricsFromFile(
    String path, {
    int fftSize = 2048,
    int hop = 512,
  }) async {
    requested.add(path);
    return null;
  }

  @override
  Future<TakeAnalysisBundle?> computeBothFromFile(
    String path, {
    int fftSize = 2048,
    int hop = 512,
    double maxHz = 5000,
  }) async {
    requested.add(path);
    final g = gate;
    if (g != null) await g.future;
    return null;
  }
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

  /// Set by [invalidatePendingStart] (a Stop in the arm window).
  bool _invalidated = false;

  @override
  Future<MicRecordingStart> start(RecordingPathStrategy destination) async {
    _starting = true;
    _invalidated = false;
    try {
      _path = await destination.create();
      final r = ready;
      if (r != null) await r.future;
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

AudioTrack _backing() => AudioTrack(
  id: 'acc',
  kind: AudioTrackKind.accompaniment,
  displayName: 'Backing',
  audioFilePath: '/pool/acc.wav',
  durationMs: 5000,
  createdAt: DateTime(2026),
);

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
  late _FakeSpectrograms spectrograms;

  /// A detail screen over the current [repo], loading in the background.
  SongPracticeDetailViewModel openVm({
    PlaybackEngine? engine,
    MicRecordingController? mic,
  }) => SongPracticeDetailViewModel(
    repository: repo,
    techniqueRepository: _FakeTechRepo(),
    audioFileService: _FakeFiles(),
    waveformService: waveforms,
    spectrogramService: spectrograms,
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

  Future<SongPracticeDetailViewModel> pumpVm({
    SongPractice? seed,
    PlaybackEngine? engine,
    MicRecordingController? mic,
  }) async {
    repo = _FakeRepo(seed ?? _seed());
    waveforms = _FakeWaveforms();
    spectrograms = _FakeSpectrograms();
    final vm = openVm(engine: engine, mic: mic);
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

  group('interpretations wait out the take window', () {
    test('a switch while the mic is arming is refused; the take lands on '
        'screen', () async {
      final mic = _FakeMic()..ready = Completer<void>();
      final vm = await pumpVm(engine: _FakeGraphEngine(), mic: mic);
      vm.selectTrack('take1');
      final start = vm.recording.startRecording();
      await Future<void>.delayed(const Duration(milliseconds: 10));
      expect(vm.recording.isStartingTake, isTrue);
      await vm.versions.setActive('v2');
      expect(vm.versions.active?.id, 'v1');
      mic.ready!.complete();
      await start;
      await vm.stopRecording();
      expect(
        vm.versions.vocalTakes.singleWhere((t) => t.id == 'take1').clips,
        hasLength(2),
      );
    });

    test('create and delete inside the stop window are refused', () async {
      final mic = _FakeMic();
      final vm = await pumpVm(engine: _FakeGraphEngine(), mic: mic);
      vm.selectTrack('take1');
      await vm.recording.startRecording();
      final stop = vm.stopRecording();
      expect(vm.recording.isStopping, isTrue);
      final create = vm.versions.create('Late');
      final delete = vm.versions.delete('v1');
      expect(vm.busy, isFalse);
      await Future.wait([create, delete, stop]);
      expect(repo.stored.versions.map((v) => v.id), ['v1', 'v2']);
      expect(repo.stored.activeVersionId, 'v1');
      expect(
        repo.stored.vocalTracks.singleWhere((t) => t.id == 'take1').clips,
        hasLength(2),
      );
    });
  });

  group("REC's empty take", () {
    test(
      'a failed save aborts the start instead of arming the backing',
      () async {
        final engine = _FakeGraphEngine();
        final vm = await pumpVm(
          seed: _seed(accompaniment: _backing()),
          engine: engine,
          mic: _FakeMic(),
        );
        expect(vm.selectedTrackId, 'acc'); // the load selects the backing lane
        expect(vm.recording.recordTargetId, isNull); // nothing armed
        repo.failNextSave = true; // the empty take's save fails
        await vm.recording.startRecording();
        expect(vm.isRecording, isFalse);
        expect(
          engine.muted['acc-c0'],
          isNot(isTrue),
          reason: 'the backing was frozen as the punch-in target and muted',
        );
        expect(vm.practice!.vocalTracks.map((t) => t.id), ['take1']);
      },
    );

    test('a REC cancelled while arming gives the selection back', () async {
      // REC selected the empty take it made (and cleared a bus selection);
      // dropping the take fell back to the first lane instead.
      final mic = _FakeMic()..ready = Completer<void>();
      final vm = await pumpVm(engine: _FakeGraphEngine(), mic: mic);
      final busId = await vm.mixer.createBus();
      expect(vm.selectedBusId, busId);
      final start = vm.recording.startRecording();
      await Future<void>.delayed(const Duration(milliseconds: 10));
      expect(vm.selectedBusId, isNull, reason: 'the empty take took it');
      await vm.stopRecording();
      mic.ready!.complete();
      await start;
      expect(vm.practice!.vocalTracks.map((t) => t.id), ['take1']);
      expect(vm.selectedBusId, busId);
    });

    test('a REC cancelled while arming leaves no empty take behind', () async {
      final mic = _FakeMic()..ready = Completer<void>();
      final vm = await pumpVm(
        seed: _seed(accompaniment: _backing()),
        engine: _FakeGraphEngine(),
        mic: mic,
      );
      expect(vm.recording.recordTargetId, isNull); // nothing armed
      final start = vm.recording.startRecording();
      await Future<void>.delayed(const Duration(milliseconds: 10));
      expect(vm.practice!.vocalTracks, hasLength(2), reason: 'REC made one');
      await vm.stopRecording();
      mic.ready!.complete();
      await start;
      expect(vm.isRecording, isFalse);
      expect(vm.practice!.vocalTracks.map((t) => t.id), ['take1']);
      expect(repo.stored.vocalTracks.map((t) => t.id), ['take1']);
    });
  });

  test('a folded-out lane re-reads a clip whose path a save moved', () async {
    // A synced clip opens at its `asset://` ref; once its bytes land, a save
    // re-reads it at the pool path, which re-keys the lane's analysis. The
    // folded-out lane computes only when pushed, so it sat blank with no
    // spinner until it was folded again.
    final take = AudioTrack(
      id: 'take1',
      kind: AudioTrackKind.vocal,
      displayName: 'Take 1',
      clips: const [
        AudioClip(id: 'c1', audioFilePath: '/pool/a.wav', durationMs: 1000),
        AudioClip(
          id: 'c2',
          audioFilePath: 'asset://b.wav',
          durationMs: 1000,
          startMs: 2000,
        ),
      ],
      createdAt: DateTime(2026),
      versionId: 'v1',
    );
    final vm = await pumpVm(seed: _seed(vocalTracks: [take]));
    vm.analysis.toggleTrackExpanded('take1');
    await Future<void>.delayed(const Duration(milliseconds: 20));
    repo.pooling = true;
    spectrograms.requested.clear();

    await vm.renameTrack('take1', 'Lead'); // any save re-reads the practice
    await Future<void>.delayed(const Duration(milliseconds: 20));
    expect(spectrograms.requested, contains('/pool/b.wav'));
  });

  group('a take that becomes selected replaces a bus selection', () {
    test('REC with a bus selected: the new take, not the bus, owns the '
        'inspector', () async {
      final vm = await pumpVm(engine: _FakeGraphEngine(), mic: _FakeMic());
      final busId = await vm.mixer.createBus();
      expect(vm.selectedBusId, busId);
      await vm.recording.startRecording();
      await vm.stopRecording();
      final take = vm.practice!.vocalTracks.last;
      expect(take.id, isNot('take1'));
      expect(take.clips, hasLength(1));
      expect(vm.selectedTrackId, take.id);
      expect(vm.selectedBusId, isNull);
    });

    test('a clip dropped onto a take with a bus selected', () async {
      final vm = await pumpVm();
      await vm.mixer.createBus();
      await vm.addClipToTake('take1', '/src/drop.wav');
      expect(vm.selectedTrackId, 'take1');
      expect(vm.selectedBusId, isNull);
    });
  });

  group('a clip dragged while a long edit runs keeps its new position', () {
    AudioTrack take(String id, String name) => AudioTrack(
      id: id,
      kind: AudioTrackKind.vocal,
      displayName: name,
      audioFilePath: '/pool/$id.wav',
      durationMs: 1000,
      createdAt: DateTime(2026),
      versionId: 'v1',
    );

    test('split: both halves follow the dragged clip', () async {
      final vm = await pumpVm();
      vm.position.value = const Duration(milliseconds: 500);
      waveforms.gate = Completer<void>(); // the halves are being analyzed
      final split = vm.splitTrackAtPlayhead('take1');
      await Future<void>.delayed(const Duration(milliseconds: 5));
      vm.moveClipStartLive('take1', 'take1-c0', 300);
      await vm.commitTrackMove();
      waveforms.gate!.complete();
      await split;

      final first = repo.stored.vocalTracks[0];
      final second = repo.stored.vocalTracks[1];
      expect(first.id, 'take1');
      expect(first.clips.single.startMs, 300, reason: 'the drag snapped back');
      expect(second.clips.single.startMs, 800);
    });

    test(
      'merge: the merged clip sits where the top clip was dragged',
      () async {
        final vm = await pumpVm(
          seed: _seed(
            vocalTracks: [take('top', 'Top'), take('bottom', 'Bottom')],
          ),
        );
        waveforms.gate = Completer<void>(); // the merged file is being analyzed
        final merge = vm.mergeTrackDown('top');
        await Future<void>.delayed(const Duration(milliseconds: 5));
        vm.moveClipStartLive('top', 'top-c0', 300);
        await vm.commitTrackMove();
        waveforms.gate!.complete();
        await merge;

        expect(repo.stored.vocalTracks.map((t) => t.id), ['top']);
        expect(repo.stored.vocalTracks.single.clips.single.startMs, 300);
      },
    );

    test('re-analyze: the analyzed clip keeps the dragged position', () async {
      final vm = await pumpVm();
      waveforms.gate = Completer<void>();
      final reanalyze = vm.reanalyzeTrack('take1');
      await Future<void>.delayed(const Duration(milliseconds: 5));
      vm.moveClipStartLive('take1', 'take1-c0', 300);
      await vm.commitTrackMove();
      waveforms.gate!.complete();
      await reanalyze;

      expect(repo.stored.vocalTracks.single.clips.single.startMs, 300);
    });
  });

  group('the dock follows a clip that just landed', () {
    // Inspector collapsed, dock open on an analysis tab: the dock computes
    // only when pushed, so the push must name the clip as the model shows it.
    Future<SongPracticeDetailViewModel> pumpDockOnly() async {
      final vm = await pumpVm(engine: _FakeGraphEngine(), mic: _FakeMic());
      repo.pooling = true;
      vm.toggleInspectorPanel();
      expect(vm.inspectorOpen, isFalse);
      vm.analysis.showDockTab(DockTab.spectrogram);
      return vm;
    }

    test('a recording punched into a take', () async {
      final vm = await pumpDockOnly();
      vm.selectTrack('take1');
      await vm.recording.startRecording();
      await vm.stopRecording();
      final clip = vm.selectedTrack!.clipById(vm.selectedClipId!)!;
      expect(clip.audioFilePath, '/pool/rec0.wav');
      expect(spectrograms.requested, contains('/pool/rec0.wav'));
    });

    test('a clip dropped onto a take', () async {
      final vm = await pumpDockOnly();
      await vm.addClipToTake('take1', '/src/drop.wav');
      final clip = vm.selectedTrack!.clipById(vm.selectedClipId!)!;
      expect(clip.audioFilePath, '/pool/import0.wav');
      expect(spectrograms.requested, contains('/pool/import0.wav'));
    });
  });

  group('an analysis in flight for a take that is removed', () {
    test('is discarded, not run again on the dead file', () async {
      // The load selects the backing lane, which carries no analysis.
      final vm = await pumpVm(seed: _seed(accompaniment: _backing()));
      expect(spectrograms.requested, isEmpty);
      spectrograms.gate = Completer<void>();
      vm.selectTrack('take1'); // the inspector starts analyzing it
      await Future<void>.delayed(const Duration(milliseconds: 5));
      expect(spectrograms.requested, ['/pool/take1.wav']);
      await vm.deleteTrack('take1');
      expect(vm.practice!.vocalTracks, isEmpty);
      expect(spectrograms.gate!.isCompleted, isFalse, reason: 'in flight');
      spectrograms.gate!.complete();
      await Future<void>.delayed(const Duration(milliseconds: 20));
      expect(spectrograms.requested, ['/pool/take1.wav']);
    });

    test('for a path another lane still plays is kept', () async {
      // Identical bytes resolve to ONE pool path, so a duplicated take (or a
      // file imported twice) has both lanes at the same path. Deleting one
      // evicted the other's analysis and disowned its compute in flight,
      // leaving the dock with no result and no loading state until the lane
      // was selected again.
      AudioTrack shared(String id) => AudioTrack(
        id: id,
        kind: AudioTrackKind.vocal,
        displayName: id,
        audioFilePath: '/pool/shared.wav',
        durationMs: 1000,
        createdAt: DateTime(2026),
        versionId: 'v1',
      );
      final vm = await pumpVm(
        seed: _seed(
          accompaniment: _backing(),
          vocalTracks: [shared('a'), shared('b')],
        ),
      );
      // The dock alone (inspector collapsed) computes only on pushes.
      vm.toggleInspectorPanel();
      vm.analysis.showDockTab(DockTab.spectrogram);
      spectrograms.gate = Completer<void>();
      vm.selectTrack('a');
      await Future<void>.delayed(const Duration(milliseconds: 5));
      expect(spectrograms.requested, ['/pool/shared.wav']);

      await vm.deleteTrack('b');
      expect(vm.selectedTrackId, 'a');
      spectrograms.gate!.complete();
      await Future<void>.delayed(const Duration(milliseconds: 20));
      // The compute landed for the lane still on screen (this fake answers
      // null, which latches "unavailable").
      expect(
        vm.analysis.spectrogramLoading ||
            vm.analysis.selectedMetricsUnavailable ||
            vm.analysis.selectedSpectrogram != null,
        isTrue,
        reason: 'the dock was left with nothing to show',
      );
    });
  });

  group('a clip dropped onto a take', () {
    test(
      'lands where the playhead was at the drop, not after the import',
      () async {
        final vm = await pumpVm();
        vm.position.value = const Duration(milliseconds: 700);
        waveforms.gate = Completer<void>(); // the import is being analyzed
        final drop = vm.addClipToTake('take1', '/src/drop.wav');
        await Future<void>.delayed(const Duration(milliseconds: 5));
        // Playback (or a scrub) moves the playhead on meanwhile.
        vm.position.value = const Duration(milliseconds: 4000);
        waveforms.gate!.complete();
        await drop;
        final take1 = repo.stored.vocalTracks.single;
        expect(take1.clips, hasLength(2));
        expect(take1.clips.last.startMs, 700);
      },
    );
  });

  test('a practice reopened while its last take is landing shows the '
      'take', () async {
    // Stop, then Back: the route no longer waits once isRecording is false,
    // and the take's finalize (its analysis, then the save) outlives the
    // screen. Reopening the practice inside those seconds read it without the
    // take, which then appeared only after some later edit saved.
    repo = _FakeRepo(_seed());
    waveforms = _FakeWaveforms();
    spectrograms = _FakeSpectrograms();
    final first = openVm(engine: _FakeGraphEngine(), mic: _FakeMic());
    for (var i = 0; i < 400 && first.practice == null; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }
    first.selectTrack('take1');
    await first.recording.startRecording();
    expect(first.isRecording, isTrue);

    waveforms.gate = Completer<void>(); // the take is being analyzed
    final stop = first.stopRecording();
    first.dispose();

    final reopened = openVm();
    addTearDown(reopened.dispose);
    await Future<void>.delayed(const Duration(milliseconds: 20));
    waveforms.gate!.complete();
    await stop;
    for (var i = 0; i < 400 && reopened.practice == null; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }

    final take1 = reopened.practice!.vocalTracks.singleWhere(
      (t) => t.id == 'take1',
    );
    expect(take1.clips, hasLength(2), reason: 'the take it was reopened on');
  });

  test('a practice reopened while an edit is still saving shows the '
      'edit', () async {
    // An edit's save outlives the screen the same way a take's finalize does
    // (rename, then Back while the disk is slow): the reopened screen read the
    // practice from before the edit, and its next save then merged against
    // that stale base.
    repo = _FakeRepo(_seed());
    waveforms = _FakeWaveforms();
    spectrograms = _FakeSpectrograms();
    final first = openVm();
    for (var i = 0; i < 400 && first.practice == null; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }
    repo.saveGate = Completer<void>();
    final rename = first.renameTrack('take1', 'Lead');
    await Future<void>.delayed(const Duration(milliseconds: 5));
    first.dispose();

    final reopened = openVm();
    addTearDown(reopened.dispose);
    await Future<void>.delayed(const Duration(milliseconds: 20));
    repo.saveGate!.complete();
    await rename;
    for (var i = 0; i < 400 && reopened.practice == null; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }

    final take1 = reopened.practice!.vocalTracks.singleWhere(
      (t) => t.id == 'take1',
    );
    expect(take1.displayName, 'Lead', reason: 'the edit it was reopened on');
  });
}
