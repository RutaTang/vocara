import 'dart:async';
import 'dart:io';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/repositories/song_practice_repository.dart';
import 'package:vocara/data/repositories/technique_repository.dart';
import 'package:vocara/data/services/app_settings_service.dart';
import 'package:vocara/data/services/audio_dsp.dart';
import 'package:vocara/data/services/audio_edit_service.dart';
import 'package:vocara/data/services/audio_file_service.dart';
import 'package:vocara/data/services/graph_audio_playback_service.dart'
    show maxPlaybackDuration;
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/data/services/playback_engine.dart';
import 'package:vocara/data/services/source_separation_service.dart';
import 'package:vocara/data/services/spectrogram_service.dart';
import 'package:vocara/data/services/waveform_service.dart';
import 'package:vocara/data/sync/sync_engine.dart';
import 'package:vocara/data/sync/sync_hashes.dart';
import 'package:vocara/domain/models/audio_fx.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/lyric_line.dart';
import 'package:vocara/domain/models/practice_note.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/ui/song_practice_detail/view_model/song_practice_detail_view_model.dart';
import 'package:vocara/utils/result.dart';

/// In-memory repository: [stored] plays the on-disk aggregate, every write
/// appends 'save' to the shared [events] log (interleaved with the file
/// fakes' deletes, so save-vs-delete ORDER is assertable), and the in-flight
/// counter proves saves are serialized.
class _FakeRepo extends Fake implements SongPracticeRepository {
  _FakeRepo(this.stored, this.events);

  SongPractice stored;
  final List<String> events;
  bool failSaves = false;

  /// Fails only the NEXT save, then heals — models a transient write error
  /// with later saves succeeding (the queued-edit survival test).
  bool failNextSave = false;
  int saveNo = 0;
  Set<int> failAt = {};
  int inFlight = 0;
  int maxInFlight = 0;

  /// When set, loads park on it — a remote reload's read held in flight.
  Completer<void>? loadGate;

  /// When set, the next save comes back merged with these words from a peer.
  List<LyricLine>? peerWordsOnNextSave;

  /// When set, saves park on it — a write held in flight.
  Completer<void>? saveGate;

  /// The next load fails.
  bool failNextLoad = false;

  /// When set, the next load reads the store, then parks on it before it
  /// answers — a read taken before a change that lands while it is out.
  Completer<void>? staleLoadGate;

  @override
  Future<Result<Loaded<SongPractice>>> loadPractice(String id) async {
    final stale = staleLoadGate;
    if (stale != null) {
      staleLoadGate = null;
      final read = stored;
      await stale.future;
      return Result.ok(Loaded(read, const {}));
    }
    final gate = loadGate;
    if (gate != null) await gate.future;
    if (failNextLoad) {
      failNextLoad = false;
      return Result.error(Exception('load failed'));
    }
    return Result.ok(Loaded(stored, const {}));
  }

  @override
  Future<Result<Loaded<SongPractice>>> savePracticeWithBase(
    SongPractice practice, {
    required Map<String, String> baseHashes,
  }) async {
    inFlight++;
    if (inFlight > maxInFlight) maxInFlight = inFlight;
    final gate = saveGate;
    if (gate != null) await gate.future;
    // Yield, so an overlapping (non-serialized) second save would be caught
    // by the in-flight counter.
    await Future<void>.delayed(Duration.zero);
    inFlight--;
    saveNo++;
    if (failSaves || failNextSave || failAt.contains(saveNo)) {
      failNextSave = false;
      events.add('save-failed');
      return Result.error(Exception('save failed'));
    }
    final peer = peerWordsOnNextSave;
    if (peer != null) {
      peerWordsOnNextSave = null;
      // A concurrent writer's lyrics, kept by the three-way merge.
      stored = practice.copyWith(lyrics: peer);
      events.add('save-merged');
      return Result.ok(
        Loaded(
          stored,
          const {'lyrics': 'peer'},
          sentHashes: const {'lyrics': 'mine'},
        ),
      );
    }
    stored = practice;
    events.add('save');
    return Result.ok(Loaded(stored, const {}));
  }
}

class _FakeTechRepo extends Fake implements TechniqueRepository {}

/// File fake: imports yield after a microtask (so a racing second action can
/// try to slip in) and deletes land in the shared [events] log.
class _FakeFiles extends Fake implements AudioFileService {
  _FakeFiles(this.events);

  final List<String> events;
  int imports = 0;

  /// When set, the next import fails reading its source (moved, a
  /// disconnected drive).
  bool failNextRead = false;

  @override
  Future<String> importAudio({
    required String practiceId,
    required String sourcePath,
  }) async {
    await Future<void>.delayed(Duration.zero);
    if (failNextRead) {
      failNextRead = false;
      throw FileSystemException('Cannot open file', sourcePath);
    }
    return '/import/${imports++}.wav';
  }

  @override
  Future<void> deleteAudio(String path) async => events.add('delete:$path');
}

/// Waveform fake that always fails — the injected post-import failure for
/// asserting the sandbox copy is rolled back.
class _ThrowingWaveforms extends Fake implements WaveformService {
  @override
  Future<WaveformResult> analyze(
    String audioFilePath, {
    int targetBuckets = 1000,
  }) async => throw StateError('injected analyze failure');
}

/// Edits fake whose transcode always fails — the injected failure for the
/// transcode-rollback test.
class _ThrowingEdits extends _FakeEdits {
  @override
  Future<String> ensureStreamableWav({
    required String practiceId,
    required String path,
  }) async => throw const FormatException('injected transcode failure');
}

class _FakeWaveforms extends Fake implements WaveformService {
  @override
  Future<WaveformResult> analyze(
    String audioFilePath, {
    int targetBuckets = 1000,
  }) async => (peaks: const <double>[], durationMs: 1000);
}

/// Waveform analysis that parks until the test releases it — a stand-in for
/// the seconds a real analyze takes, during which the UI stays interactive.
class _GatedWaveforms extends Fake implements WaveformService {
  final Completer<void> gate = Completer<void>();

  @override
  Future<WaveformResult> analyze(
    String audioFilePath, {
    int targetBuckets = 1000,
  }) async {
    await gate.future;
    return (peaks: const <double>[], durationMs: 1000);
  }
}

/// [_GatedWaveforms] that says when an analysis has reached its gate.
class _SignalingWaveforms extends _GatedWaveforms {
  final Completer<void> reached = Completer<void>();

  @override
  Future<WaveformResult> analyze(
    String audioFilePath, {
    int targetBuckets = 1000,
  }) {
    if (!reached.isCompleted) reached.complete();
    return super.analyze(audioFilePath, targetBuckets: targetBuckets);
  }
}

/// Separation that parks until the test releases it — the minutes-long window
/// during which the rest of the screen stays interactive.
class _GatedSeparation implements SourceSeparationService {
  final Completer<void> gate = Completer<void>();

  @override
  Future<bool> isAvailable() async => true;

  @override
  void cancel() {}

  @override
  Future<SeparationResult> separate(
    String sourcePath, {
    void Function(double progress)? onProgress,
  }) async {
    onProgress?.call(0.2);
    await gate.future;
    return (instrumentalPath: '/stems/inst.wav', vocalPath: '/stems/voc.wav');
  }
}

class _FakeEdits extends Fake implements AudioEditService {
  /// Paths handed to [ensureStreamableWav], for transcode-call assertions.
  final List<String> streamableCalls = [];

  /// When set, [ensureStreamableWav] reports this path as the transcoded
  /// WAV (modelling a compressed import); null = passthrough (already WAV).
  String? transcodeTo;

  @override
  Future<String> ensureStreamableWav({
    required String practiceId,
    required String path,
  }) async {
    streamableCalls.add(path);
    return transcodeTo ?? path;
  }

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

  int _dups = 0;

  /// When set, the duplicate call at this 0-based index throws — a copy that
  /// fails PART WAY through a multi-clip lane.
  int? failDuplicateAt;

  /// When set, each duplicate parks on it — a copy of a long take.
  Completer<void>? duplicateGate;

  @override
  Future<String> duplicate({
    required String practiceId,
    required String sourcePath,
  }) async {
    final gate = duplicateGate;
    if (gate != null) await gate.future;
    final n = _dups++;
    if (n == failDuplicateAt) {
      throw StateError('injected duplicate failure (disk full)');
    }
    return '/edit/dup$n.wav';
  }
}

/// A do-nothing engine (mirrors export_take_test's fake): just enough surface
/// for the playback/mixer controllers to wire up without native audio.
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
  void setTrackPan(String id, double pan) {}
  @override
  void setMasterVolume(double gain) {}
  @override
  void setTrackFx(String trackId, TrackFx fx) {}
  @override
  void setBusFx(String busId, TrackFx fx) {}
  @override
  void setBusVolume(String busId, double volume, {bool muted = false}) {}
  @override
  void setBusPan(String busId, double pan) {}
  @override
  double busGainReductionDb(String busId) => 0;
  @override
  void setTrackSendLevel(String trackId, int slot, double levelDb) {}
  @override
  void setBusSendLevel(String busId, int slot, double levelDb) {}
  @override
  void setReverb(ReverbSettings reverb) {}
  @override
  double trackGainReductionDb(String trackId) => 0;
  @override
  Future<void> dispose() async {}
}

/// Records the file paths of the last graph sync.
class _SyncRecordingEngine extends _FakeEngine {
  List<String> lastPaths = const [];

  @override
  Future<void> sync(
    List<TrackPlayback> tracks,
    List<BusPlayback> buses,
  ) async => lastPaths = [for (final t in tracks) t.path];
}

/// Engine that remembers the last mute pushed for each source — through a
/// full graph sync or a live per-source level push, whichever came last.
class _MuteTrackingEngine extends _FakeEngine {
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
}

SongPractice _seedPractice({
  List<AudioTrack>? vocalTracks,
  AudioTrack? reference,
  String? separationSource,
}) => SongPractice(
  id: 'p1',
  title: 'Song',
  createdAt: DateTime(2026),
  updatedAt: DateTime(2026),
  reference: reference,
  separationSource: separationSource,
  lyrics: const [LyricLine(text: 'la la la')],
  vocalTracks:
      vocalTracks ??
      [
        AudioTrack(
          id: 'single',
          kind: AudioTrackKind.vocal,
          displayName: 'Take 1',
          audioFilePath: '/pool/take1.wav',
          durationMs: 1000,
          createdAt: DateTime(2026),
        ),
        AudioTrack(
          id: 'multi',
          kind: AudioTrackKind.vocal,
          displayName: 'Take 2',
          createdAt: DateTime(2026),
          clips: [
            AudioClip(
              id: 'clip-a',
              audioFilePath: '/pool/clipA.wav',
              durationMs: 400,
            ),
            AudioClip(
              id: 'clip-b',
              audioFilePath: '/pool/clipB.wav',
              durationMs: 400,
              startMs: 500,
            ),
          ],
        ),
      ],
);

/// Two adjacent single-clip takes, so mergeTrackDown('top') is legal.
List<AudioTrack> _mergeableTakes() => [
  AudioTrack(
    id: 'top',
    kind: AudioTrackKind.vocal,
    displayName: 'Top',
    audioFilePath: '/pool/top.wav',
    durationMs: 1000,
    createdAt: DateTime(2026),
  ),
  AudioTrack(
    id: 'bottom',
    kind: AudioTrackKind.vocal,
    displayName: 'Bottom',
    audioFilePath: '/pool/bottom.wav',
    durationMs: 1000,
    createdAt: DateTime(2026),
  ),
];

void main() {
  late List<String> events;
  late _FakeRepo repo;
  late _FakeFiles files;

  Future<SongPracticeDetailViewModel> pumpVm({
    List<AudioTrack>? vocalTracks,
    Duration? Function(String path)? probeDuration,
    WaveformService? waveforms,
    _FakeEdits? edits,
    SourceSeparationService? separation,
    AudioTrack? reference,
    String? separationSource,
    Stream<RemoteApplied>? remoteApplied,
    PlaybackEngine? engine,
    Completer<void>? firstLoad,
  }) async {
    events = <String>[];
    repo = _FakeRepo(
      _seedPractice(
        vocalTracks: vocalTracks,
        reference: reference,
        separationSource: separationSource,
      ),
      events,
    )..staleLoadGate = firstLoad;
    files = _FakeFiles(events);
    final storage = LocalStorageService();
    final vm = SongPracticeDetailViewModel(
      repository: repo,
      techniqueRepository: _FakeTechRepo(),
      audioFileService: files,
      waveformService: waveforms ?? _FakeWaveforms(),
      spectrogramService: const SpectrogramService(),
      separationService:
          separation ?? FakeSourceSeparationService(available: false),
      appSettings: AppSettingsService(storage: storage),
      practiceId: 'p1',
      playbackEngine: engine ?? _FakeEngine(),
      editService: edits ?? _FakeEdits(),
      probeDuration: probeDuration,
      remoteApplied: remoteApplied,
    );
    addTearDown(vm.dispose);
    if (firstLoad != null) return vm; // the caller lets it land
    for (var i = 0; i < 400 && vm.practice == null; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }
    expect(vm.practice, isNotNull, reason: 'initial load should complete');
    events.clear();
    return vm;
  }

  group('imports kept consistent', () {
    test('a screen already gone imports nothing more', () async {
      // A drop of several files carried on importing after Back.
      final vm = await pumpVm();
      final before = repo.stored.vocalTracks.length;
      vm.dispose();
      await vm.addVocalTake('/src/late.wav');
      await vm.addAccompaniment('/src/late.wav');
      await vm.addReference('/src/late.wav');
      expect(await vm.addClipToTake('single', '/src/late.wav'), isNull);
      expect(files.imports, 0);
      expect(repo.stored.vocalTracks, hasLength(before));
    });

    test(
      'an imported take leaves the selection, REC\'s target, alone',
      () async {
        // Selected, a guide vocal imported to sing along with became the take
        // the next recording landed on, muted for the whole of it.
        final vm = await pumpVm();
        final before = vm.selectedTrackId;
        final target = vm.recording.recordTargetId;
        await vm.addVocalTake('/src/guide.wav');
        expect(vm.selectedTrackId, before);
        expect(vm.recording.recordTargetId, target);
      },
    );

    test(
      'a lyrics "file" that is a pipe or a device is refused at once',
      () async {
        // Their length reads 0, so the size cap passed and the read went on
        // without end: a pipe held the import open, /dev/zero ate memory.
        final dir = Directory.systemTemp.createTempSync('lyrics_dev');
        addTearDown(() => dir.deleteSync(recursive: true));
        final pipe = '${dir.path}/pipe.lrc';
        expect((await Process.run('mkfifo', [pipe])).exitCode, 0);
        final zero = '${dir.path}/zero.lrc';
        Link(zero).createSync('/dev/zero');
        final toasts = <String>[];
        final vm = await pumpVm();
        final sub = vm.toasts.listen(toasts.add);
        addTearDown(sub.cancel);
        final before = vm.lyrics;
        for (final path in [pipe, zero]) {
          await vm
              .importLyricsFromFile(path)
              .timeout(const Duration(seconds: 3));
        }
        await Future<void>.delayed(Duration.zero);
        expect(toasts, hasLength(2));
        expect(vm.lyrics, before);
      },
    );

    test('lyrics that repeat their lines past reason are refused, not '
        'expanded', () async {
      final toasts = <String>[];
      final vm = await pumpVm();
      final sub = vm.toasts.listen(toasts.add);
      addTearDown(sub.cancel);
      final crafted = '${'[0:00]' * 40000}${'x' * 100000}';
      expect(await vm.applyEditedLyrics(crafted), isFalse);
      await Future<void>.delayed(Duration.zero);
      expect(toasts.single, contains('too many times'));
    });

    test('a file just over the length limit says so', () async {
      // Rounded down, a file just over the limit read as exactly it.
      final toasts = <String>[];
      final vm = await pumpVm(
        probeDuration: (_) =>
            maxPlaybackDuration + const Duration(milliseconds: 400),
      );
      final sub = vm.toasts.listen(toasts.add);
      addTearDown(sub.cancel);
      await vm.addVocalTake('/src/long.wav');
      await Future<void>.delayed(Duration.zero);
      final over = maxPlaybackDuration.inMinutes + 1;
      expect(toasts.single, startsWith('That file is $over minutes long.'));
    });

    test('a file that cannot be read says so', () async {
      final toasts = <String>[];
      final vm = await pumpVm();
      final sub = vm.toasts.listen(toasts.add);
      addTearDown(sub.cancel);
      files.failNextRead = true;
      await vm.addVocalTake('/src/gone.wav');
      await Future<void>.delayed(Duration.zero);
      expect(toasts, ["The file couldn't be read."]);
    });

    test('a failed write does not blame the file', () async {
      // Every file-system error read as the source's: a full disk too.
      final dir = await Directory.systemTemp.createTemp('import_src');
      addTearDown(() => dir.delete(recursive: true));
      final source = File('${dir.path}/take.wav')..writeAsBytesSync([1, 2]);
      final toasts = <String>[];
      final vm = await pumpVm();
      final sub = vm.toasts.listen(toasts.add);
      addTearDown(sub.cancel);
      files.failNextRead = true;
      await vm.addVocalTake(source.path);
      await Future<void>.delayed(Duration.zero);
      expect(toasts.single, isNot("The file couldn't be read."));
    });
  });

  group('parallel paths kept consistent', () {
    test('a failed lyric write takes the engine back with the model', () async {
      // An import's save failed while a lyric stamp had moved the model on,
      // so it was not reverted; the stamp's failure then reverted the model to
      // the disk's, without the import's lane, but the engine kept playing it.
      final engine = _SyncRecordingEngine();
      final vm = await pumpVm(engine: engine);
      repo.saveGate = Completer<void>();
      final importing = vm.addVocalTake('/src/new.wav');
      for (var i = 0; i < 200 && repo.inFlight == 0; i++) {
        await Future<void>.delayed(const Duration(milliseconds: 1));
      }
      final stamping = vm.stampNextLyric();
      repo.failSaves = true;
      repo.saveGate!.complete();
      repo.saveGate = null;
      await importing;
      await stamping;
      for (var i = 0; i < 20; i++) {
        await Future<void>.delayed(const Duration(milliseconds: 2));
      }
      final shown = [
        for (final t in vm.practice!.vocalTracks)
          ...t.clips.map((c) => c.audioFilePath),
      ];
      expect(shown, isNot(contains('/import/0.wav')));
      expect(engine.lastPaths, isNot(contains('/import/0.wav')));
    });

    test('deleting the Original\'s clip removes the Original', () async {
      // It left an empty backing lane nothing could fill, its kept original
      // still set.
      final vm = await pumpVm(
        reference: AudioTrack(
          id: 'ref',
          kind: AudioTrackKind.reference,
          displayName: 'Original',
          audioFilePath: '/pool/orig.wav',
          durationMs: 1000,
          createdAt: DateTime(2026),
        ),
        separationSource: '/pool/orig.wav',
      );
      final clipId = vm.practice!.reference!.clips.single.id;
      await vm.deleteClip('ref', clipId);
      expect(vm.practice!.reference, isNull);
      expect(vm.practice!.separationSource, isNull);
    });

    test('a new backing keeps the file the Original still plays', () async {
      // The kept original IS the Original lane's file: deleted with the
      // replaced backing, only the pool's own refusal had saved it.
      final vm = await pumpVm(
        reference: AudioTrack(
          id: 'ref',
          kind: AudioTrackKind.reference,
          displayName: 'Original',
          audioFilePath: '/work/orig.wav',
          durationMs: 1000,
          createdAt: DateTime(2026),
        ),
        separationSource: '/work/orig.wav',
      );
      await vm.addAccompaniment('/src/backing.wav');
      expect(
        vm.practice!.reference!.clips.single.audioFilePath,
        '/work/orig.wav',
      );
      expect(events, isNot(contains('delete:/work/orig.wav')));
    });

    test('a failed empty take gives the selection back', () async {
      // It fell to the first lane, losing the selected bus, unlike a REC
      // start that drops its empty take.
      final vm = await pumpVm();
      final bus = await vm.mixer.createBus();
      expect(vm.selectedBusId, bus);
      repo.failNextSave = true;
      expect(await vm.createEmptyVocalTake(), isNull);
      expect(vm.selectedBusId, bus);
    });

    test(
      'lyrics dropped during an edit are taken, as pasted ones are',
      () async {
        final edits = _FakeEdits()..duplicateGate = Completer<void>();
        final vm = await pumpVm(edits: edits);
        final duplicating = vm.duplicateTrack('single');
        await Future<void>.delayed(const Duration(milliseconds: 5));
        expect(vm.busy, isTrue);
        final dir = await Directory.systemTemp.createTemp('lyrics');
        addTearDown(() => dir.delete(recursive: true));
        final file = File('${dir.path}/song.txt')
          ..writeAsStringSync('first line\nsecond line');
        await vm.importLyricsFromFile(file.path);
        expect(vm.lyrics.map((l) => l.text), ['first line', 'second line']);
        edits.duplicateGate!.complete();
        await duplicating;
      },
    );
  });

  group('import transcode (P0 of the disk-streaming migration)', () {
    test('a compressed import is transcoded ONCE to a streamable WAV and the '
        'compressed copy is deleted', () async {
      final edits = _FakeEdits()..transcodeTo = '/edit/transcoded.wav';
      final vm = await pumpVm(edits: edits);

      await vm.addVocalTake('/src/song.mp3');
      expect(edits.streamableCalls, ['/import/0.wav']);
      expect(
        events,
        contains('delete:/import/0.wav'),
        reason: 'the compressed copy is redundant once the WAV is the track',
      );
      final added = vm.practice!.vocalTracks.last;
      expect(added.clips.single.audioFilePath, '/edit/transcoded.wav');
    });

    test('an already-WAV import passes through untouched (no re-encode, no '
        'delete)', () async {
      final edits = _FakeEdits();
      final vm = await pumpVm(edits: edits);

      await vm.addVocalTake('/src/take.wav');
      expect(edits.streamableCalls, ['/import/0.wav']);
      expect(events.where((e) => e.startsWith('delete:')), isEmpty);
      final added = vm.practice!.vocalTracks.last;
      expect(added.clips.single.audioFilePath, '/import/0.wav');
    });

    test(
      'a failed transcode rolls the import back like any failed step',
      () async {
        final edits = _ThrowingEdits();
        final vm = await pumpVm(edits: edits);
        final before = vm.practice!.vocalTracks.length;

        await vm.addVocalTake('/src/song.mp3');
        expect(events, contains('delete:/import/0.wav'));
        expect(vm.practice!.vocalTracks.length, before);
        expect(vm.errorMessage, isNotNull);
      },
    );
  });

  group('import rollback', () {
    test('a failed step AFTER the import deletes the sandbox copy instead of '
        'stranding an orphan', () async {
      // importAudio succeeded, waveform analysis throws: the official audio
      // directory used to keep the copy while nothing referenced it — repeat
      // attempts piled orphans until the sweep.
      final vm = await pumpVm(waveforms: _ThrowingWaveforms());
      final before = vm.practice!.vocalTracks.length;

      // BusyGate surfaces the failure as a toast; the future completes.
      await vm.addVocalTake('/src/a.wav');
      expect(
        events,
        contains('delete:/import/0.wav'),
        reason: 'the sandbox copy must be rolled back',
      );
      expect(vm.practice!.vocalTracks.length, before, reason: 'nothing lands');
      expect(vm.errorMessage, isNotNull, reason: 'the failure is surfaced');
    });
  });

  group('structural-edit mutex', () {
    test('an import whose declared duration exceeds the playback cap is '
        'refused before anything is copied or persisted', () async {
      final vm = await pumpVm(probeDuration: (_) => const Duration(hours: 2));
      final toasts = <String>[];
      final sub = vm.toasts.listen(toasts.add);
      addTearDown(sub.cancel);
      final before = vm.practice!.vocalTracks.length;

      await vm.addVocalTake('/src/very_long.mp3');
      await vm.addAccompaniment('/src/very_long.mp3');
      await vm.addClipToTake(vm.practice!.vocalTracks.first.id, '/src/x.mp3');
      // Let the toast stream deliver.
      await Future<void>.delayed(Duration.zero);

      // The refusal happens at the admission gate: no copy, no waveform
      // analysis, no save — the practice never learns the file existed.
      expect(events, isEmpty);
      expect(vm.practice!.vocalTracks.length, before);
      expect(vm.practice!.accompaniment, isNull);
      expect(toasts, hasLength(3));
      expect(toasts.first, contains('limit is 60 minutes'));
    });

    test('a probe that cannot run admits the import (the decode budget still '
        'bounds it downstream)', () async {
      final vm = await pumpVm(probeDuration: (_) => throw StateError('no ffi'));
      final before = vm.practice!.vocalTracks.length;

      await vm.addVocalTake('/src/a.wav');

      expect(vm.practice!.vocalTracks.length, before + 1);
    });

    test('an overlapping structural action is rejected while the gate is '
        'held (only ONE import lands)', () async {
      final vm = await pumpVm();
      final before = vm.practice!.vocalTracks.length;

      final f1 = vm.addVocalTake('/src/a.wav'); // parks inside the gate
      final f2 = vm.addVocalTake('/src/b.wav'); // guard sees busy → no-op
      await Future.wait([f1, f2]);

      expect(vm.practice!.vocalTracks.length, before + 1);
      expect(events.where((e) => e == 'save'), hasLength(1));
    });
  });

  test(
    'a split whose save fails keeps the halves the model still plays',
    () async {
      // A failed save does not revert a model an edit moved on since: the
      // split halves stayed in it, their files were deleted anyway, and the
      // rename's own save then failed on them.
      final vm = await pumpVm();
      vm.position.value = const Duration(milliseconds: 500);
      repo.saveGate = Completer<void>();
      repo.failNextSave = true;
      final split = vm.splitTrackAtPlayhead('single');
      for (var i = 0; i < 100 && repo.inFlight == 0; i++) {
        await Future<void>.delayed(Duration.zero);
      }
      final rename = vm.renameTrack('multi', 'Renamed'); // moves the model on
      final gate = repo.saveGate!;
      repo.saveGate = null;
      gate.complete();
      await Future.wait([split, rename]);

      expect(
        events.where((e) => e.startsWith('delete:/edit/')),
        isEmpty,
        reason: 'the halves are still in the model',
      );
      expect(
        repo.stored.vocalTracks
            .expand((t) => t.clips)
            .map((c) => c.audioFilePath),
        containsAll(['/edit/first.wav', '/edit/second.wav']),
      );
    },
  );

  test('deleting the Original lets go of the original kept for '
      're-extraction', () async {
    // Only the Original lane offers re-extraction: left set, the kept
    // original held its audio for good, though the dialog said it goes.
    final vm = await pumpVm(
      reference: AudioTrack(
        id: 'orig',
        kind: AudioTrackKind.reference,
        displayName: 'Original',
        audioFilePath: '/pool/orig.wav',
        durationMs: 1000,
        createdAt: DateTime(2026),
      ),
      separationSource: '/audio/p1/source.wav',
    );
    await vm.deleteTrack('orig');
    expect(repo.stored.reference, isNull);
    expect(repo.stored.separationSource, isNull);
    expect(events, contains('delete:/audio/p1/source.wav'));
  });

  group('deleting a clip from over another', () {
    // A take's notes are the lane's, at absolute times: deleting a punch-in
    // from over its original took the notes the original still played under.
    PracticeNote note(String id, int at, [int? end]) => PracticeNote(
      id: id,
      positionMs: at,
      endMs: end,
      content: id,
      createdAt: DateTime(2026),
      updatedAt: DateTime(2026),
    );

    test('keeps the notes another clip still covers', () async {
      final vm = await pumpVm(
        vocalTracks: [
          AudioTrack(
            id: 'lane',
            kind: AudioTrackKind.vocal,
            displayName: 'Take 1',
            createdAt: DateTime(2026),
            clips: [
              AudioClip(
                id: 'orig',
                audioFilePath: '/pool/o.wav',
                durationMs: 3000,
              ),
              AudioClip(
                id: 'punch',
                audioFilePath: '/pool/p.wav',
                startMs: 1000,
                durationMs: 3000,
              ),
            ],
            notes: [
              note('under-both', 1500),
              note('punch-only', 3500),
              note('range', 2000, 3800),
            ],
          ),
        ],
      );
      await vm.deleteClip('lane', 'punch');
      final lane = vm.practice!.vocalTracks.single;
      // 1000..3000 is still the original's; only 3000..4000 falls silent.
      expect(lane.notes.map((n) => n.id), ['under-both', 'range']);
      expect(lane.notes.last.endMs, 3000, reason: 'clamped at the gap');
    });

    test('the uncovered spans of a clip', () {
      AudioClip clip(int start, int dur) => AudioClip(
        id: '$start',
        audioFilePath: '/x',
        startMs: start,
        durationMs: dur,
      );
      expect(uncoveredSpans(clip(1000, 3000), [clip(0, 3000)]), [(3000, 4000)]);
      expect(uncoveredSpans(clip(0, 4000), [clip(1000, 1000)]), [
        (0, 1000),
        (2000, 4000),
      ]);
      expect(uncoveredSpans(clip(0, 1000), [clip(0, 1000)]), isEmpty);
      expect(uncoveredSpans(clip(0, 1000), [clip(2000, 1000)]), [(0, 1000)]);
    });
  });

  group('persist-before-delete invariant', () {
    test('deleteClip saves FIRST and deletes the clip audio only after the '
        'save succeeded', () async {
      final vm = await pumpVm();
      await vm.deleteClip('multi', 'clip-a');

      expect(events, ['save', 'delete:/pool/clipA.wav']);
      final multi = vm.practice!.vocalTracks.firstWhere((t) => t.id == 'multi');
      expect(multi.clips.map((c) => c.id), ['clip-b']);
    });

    test('deleteClip on a FAILED save deletes nothing and reverts to the '
        'stored model', () async {
      final vm = await pumpVm();
      final toasts = <String>[];
      vm.toasts.listen(toasts.add);
      repo.failSaves = true;

      await vm.deleteClip('multi', 'clip-a');
      await Future<void>.delayed(Duration.zero);

      expect(events.where((e) => e.startsWith('delete:')), isEmpty);
      final multi = vm.practice!.vocalTracks.firstWhere((t) => t.id == 'multi');
      expect(multi.clips.map((c) => c.id), ['clip-a', 'clip-b']);
      expect(toasts, contains("Couldn't save your changes."));
    });

    test('deleteTrack saves FIRST and deletes every clip file only after '
        'the save succeeded', () async {
      final vm = await pumpVm();
      await vm.deleteTrack('multi');

      expect(events, [
        'save',
        'delete:/pool/clipA.wav',
        'delete:/pool/clipB.wav',
      ]);
      expect(vm.practice!.vocalTracks.map((t) => t.id), ['single']);
    });

    test('deleteTrack on a FAILED save keeps every clip file', () async {
      final vm = await pumpVm();
      repo.failSaves = true;
      await vm.deleteTrack('multi');

      expect(events.where((e) => e.startsWith('delete:')), isEmpty);
      expect(vm.practice!.vocalTracks.map((t) => t.id), ['single', 'multi']);
    });

    test('split saves FIRST, then deletes only the original file', () async {
      final vm = await pumpVm();
      vm.playback.position.value = const Duration(milliseconds: 500);
      await vm.splitTrackAtPlayhead('single');

      expect(events, ['save', 'delete:/pool/take1.wav']);
      expect(vm.practice!.vocalTracks, hasLength(3));
    });

    test('split on a FAILED save keeps the original and cleans up the '
        'newly written halves instead', () async {
      final vm = await pumpVm();
      vm.playback.position.value = const Duration(milliseconds: 500);
      repo.failSaves = true;
      await vm.splitTrackAtPlayhead('single');

      final deletes = events.where((e) => e.startsWith('delete:')).toList();
      expect(deletes, ['delete:/edit/first.wav', 'delete:/edit/second.wav']);
      // Reverted to the stored model: still the two seeded takes.
      expect(vm.practice!.vocalTracks.map((t) => t.id), ['single', 'multi']);
    });

    test('merge saves FIRST, then deletes only the two source files', () async {
      final vm = await pumpVm(vocalTracks: _mergeableTakes());
      await vm.mergeTrackDown('top');

      expect(events, [
        'save',
        'delete:/pool/top.wav',
        'delete:/pool/bottom.wav',
      ]);
      expect(vm.practice!.vocalTracks.map((t) => t.id), ['top']);
    });

    test('a selected bottom take follows its audio into the merged lane, and '
        'back out of it when the save fails', () async {
      for (final fail in [false, true]) {
        final vm = await pumpVm(vocalTracks: _mergeableTakes());
        final bottomClip = vm.practice!.vocalTracks[1].clips.single.id;
        vm.selectClip('bottom', bottomClip);
        expect(vm.recording.recordTargetId, 'bottom');
        repo.failSaves = fail;
        await vm.mergeTrackDown('top');
        if (fail) {
          expect(vm.selectedTrackId, 'bottom');
          expect(vm.selectedClipId, bottomClip);
        } else {
          expect(vm.selectedTrackId, 'top', reason: 'not the backing');
          expect(
            vm.selectedClipId,
            vm.practice!.vocalTracks.single.clips.single.id,
          );
          expect(vm.recording.recordTargetId, 'top');
        }
        repo.failSaves = false;
      }
    });

    test('merge on a FAILED save keeps the sources and cleans up the '
        'merged output instead', () async {
      final vm = await pumpVm(vocalTracks: _mergeableTakes());
      repo.failSaves = true;
      await vm.mergeTrackDown('top');

      final deletes = events.where((e) => e.startsWith('delete:')).toList();
      expect(deletes, ['delete:/edit/merged.wav']);
      expect(vm.practice!.vocalTracks.map((t) => t.id), ['top', 'bottom']);
    });

    test('a split or merge whose analysis fails deletes the new files, and '
        'nothing else', () async {
      // Only a failed save cleaned up: a throw from the analysis (or the take
      // gone meanwhile) left the halves or the merged file in edits/.
      final vm = await pumpVm(waveforms: _ThrowingWaveforms());
      vm.playback.position.value = const Duration(milliseconds: 500);
      await vm.splitTrackAtPlayhead('single');
      expect(events, ['delete:/edit/first.wav', 'delete:/edit/second.wav']);
      expect(vm.practice!.vocalTracks.map((t) => t.id), ['single', 'multi']);

      final merging = await pumpVm(
        vocalTracks: _mergeableTakes(),
        waveforms: _ThrowingWaveforms(),
      );
      await merging.mergeTrackDown('top');
      expect(events, ['delete:/edit/merged.wav']);
      expect(merging.practice!.vocalTracks.map((t) => t.id), ['top', 'bottom']);
    });

    // Split and merge used to keep the surviving clip's id and point it at
    // the new file. The save then overwrote that clip row's asset in place,
    // so nothing referenced the original recording any more, and the next
    // boot's recovery adopted its staging WAV as a whole "(recovered)" take
    // beside the edited ones (see recording_recovery_test for the recovery
    // side of that contract). The edited clip must be a NEW row.
    test('split puts the surviving half in a new clip row, selection and '
        'all', () async {
      final vm = await pumpVm();
      final original = vm.practice!.vocalTracks.first.clips.single.id;
      vm.selectClip('single', original);
      vm.playback.position.value = const Duration(milliseconds: 500);
      await vm.splitTrackAtPlayhead('single');

      final first = vm.practice!.vocalTracks.firstWhere(
        (t) => t.id == 'single',
      );
      expect(first.clips.single.id, isNot(original));
      expect(vm.selectedClipId, first.clips.single.id);
    });

    test(
      'the bottom take\'s notes follow its audio in the joined file',
      () async {
        // The shift was the top clip's stored length, which a clip measured by
        // an earlier build holds up to 10 ms short of its audio.
        final dir = Directory.systemTemp.createTempSync('merge_len');
        addTearDown(() => dir.deleteSync(recursive: true));
        final topWav = File(
          '${dir.path}/top.wav',
        )..writeAsBytesSync(writeWavMono(Float64List(59232), 48000)); // 1234 ms
        final takes = _mergeableTakes();
        final vm = await pumpVm(
          vocalTracks: [
            takes[0].copyWith(
              clips: [
                takes[0].primaryClip!.copyWith(
                  audioFilePath: topWav.path,
                  durationMs: 1230,
                ),
              ],
            ),
            takes[1].copyWith(
              notes: [
                PracticeNote(
                  id: 'n',
                  positionMs: 100,
                  content: 'n',
                  createdAt: DateTime(2026),
                  updatedAt: DateTime(2026),
                ),
              ],
            ),
          ],
        );
        await vm.mergeTrackDown('top');

        expect(vm.practice!.vocalTracks.single.notes.single.positionMs, 1334);
      },
    );

    test('merge puts the merged audio in a new clip row, selection and '
        'all', () async {
      final vm = await pumpVm(vocalTracks: _mergeableTakes());
      final original = vm.practice!.vocalTracks.first.clips.single.id;
      vm.selectClip('top', original);
      await vm.mergeTrackDown('top');

      final merged = vm.practice!.vocalTracks.single;
      expect(merged.clips.single.id, isNot(original));
      expect(vm.selectedClipId, merged.clips.single.id);
    });

    // Regression guard: split/merge rebuilt their lanes from the snapshot
    // taken BEFORE the analyze awaits. Renames and note edits are not
    // busy-gated, so one landing during those seconds was overwritten — and a
    // note added meanwhile, missing from the rebuilt lane, was tombstoned by
    // the save.
    test('a rename and a note landing DURING a split survive it', () async {
      final waveforms = _GatedWaveforms();
      final vm = await pumpVm(waveforms: waveforms);
      vm.playback.position.value = const Duration(milliseconds: 500);

      final split = vm.splitTrackAtPlayhead('single');
      await Future<void>.delayed(Duration.zero); // reach the gated analyze
      await vm.renameTrack('single', 'Renamed mid-split');
      await vm.addNote('single', 100, null, 'note added mid-split');
      waveforms.gate.complete();
      await split;

      final tracks = vm.practice!.vocalTracks;
      final first = tracks.firstWhere((t) => t.id == 'single');
      expect(
        first.displayName,
        'Renamed mid-split',
        reason: 'the rename must not be overwritten by the split',
      );
      expect(
        first.notes.map((n) => n.content),
        ['note added mid-split'],
        reason: 'a note added mid-split must not be dropped (and tombstoned)',
      );
      // The new half is still derived from the same lane.
      expect(
        tracks.map((t) => t.displayName),
        contains('Renamed mid-split (2)'),
      );
    });

    test("a note's text saved from the copy its editor opened with keeps "
        'the clamp a split gave it meanwhile', () async {
      final vm = await pumpVm();
      AudioTrack single() =>
          vm.practice!.vocalTracks.firstWhere((t) => t.id == 'single');
      await vm.addNote('single', 100, 900, 'range');
      final opened = single().notes.single; // what the editor holds
      vm.playback.position.value = const Duration(milliseconds: 500);
      await vm.splitTrackAtPlayhead('single');
      final clamped = single().notes.single;
      expect(clamped.endMs, isNot(900), reason: 'precondition: split clamped');

      await vm.updateNoteContent('single', opened.id, 'edited');

      final note = single().notes.single;
      expect(note.content, 'edited');
      expect(note.endMs, clamped.endMs);
      expect(note.positionMs, clamped.positionMs);
    });

    test('a rename and a note landing DURING a merge survive it', () async {
      final waveforms = _GatedWaveforms();
      final vm = await pumpVm(
        vocalTracks: _mergeableTakes(),
        waveforms: waveforms,
      );

      final merge = vm.mergeTrackDown('top');
      await Future<void>.delayed(Duration.zero); // reach the gated analyze
      await vm.renameTrack('top', 'Renamed mid-merge');
      await vm.addNote('bottom', 200, null, 'note on the absorbed take');
      waveforms.gate.complete();
      await merge;

      final merged = vm.practice!.vocalTracks.single;
      expect(
        merged.displayName,
        'Renamed mid-merge',
        reason: 'the rename must not be overwritten by the merge',
      );
      expect(
        merged.notes.map((n) => n.content),
        ['note on the absorbed take'],
        reason: 'a note added to the absorbed take must follow its audio',
      );
      // Shifted onto the merged timeline: 200 ms in the bottom take sits
      // after the 1000 ms top take.
      expect(merged.notes.single.positionMs, 1200);
    });

    // Regression guard: separation is the longest operation in the app, and
    // the swap rebuilt the Reference lane from the snapshot taken before it
    // started — so a rename or a note added to that lane while it ran was
    // overwritten (and the note tombstoned) the moment the stems landed.
    test(
      'a rename and a note landing DURING a separation survive it',
      () async {
        final separation = _GatedSeparation();
        final reference = AudioTrack(
          id: 'ref',
          kind: AudioTrackKind.reference,
          displayName: 'Original',
          audioFilePath: '/pool/original.wav',
          durationMs: 1000,
          createdAt: DateTime(2026),
        );
        final vm = await pumpVm(separation: separation, reference: reference);
        for (var i = 0; i < 400 && !vm.separationAvailable; i++) {
          await Future<void>.delayed(const Duration(milliseconds: 5));
        }

        final run = vm.separateBacking('ref');
        await Future<void>.delayed(
          Duration.zero,
        ); // reach the parked separation
        await vm.renameTrack('ref', 'Original renamed');
        await vm.addNote('ref', 100, null, 'note during separation');
        separation.gate.complete();
        await run;

        final ref = vm.practice!.reference!;
        expect(
          ref.displayName,
          'Original renamed',
          reason: 'the rename must not be overwritten by the swap',
        );
        expect(
          ref.notes.map((n) => n.content),
          ['note during separation'],
          reason: 'a note added mid-separation must not be dropped',
        );
        // The instrumental is named from the lane's CURRENT name.
        expect(
          vm.practice!.accompaniment!.displayName,
          'Original renamed (Instrumental)',
        );
      },
    );

    // Cancel stays on screen while the stems are imported and analyzed (the
    // whole visible run on a cached song), but the service had nothing left
    // to cancel and the backing was applied anyway.
    test('the instrumental sits where the original does', () async {
      // Built without a start, it landed at 0 under an Original dragged
      // right: played early against it and against every take sung to it.
      final separation = _GatedSeparation();
      final vm = await pumpVm(
        separation: separation,
        reference: AudioTrack(
          id: 'ref',
          kind: AudioTrackKind.reference,
          displayName: 'Original',
          audioFilePath: '/pool/original.wav',
          durationMs: 1000,
          startMs: 5000,
          createdAt: DateTime(2026),
        ),
      );
      for (var i = 0; i < 400 && !vm.separationAvailable; i++) {
        await Future<void>.delayed(const Duration(milliseconds: 5));
      }
      final run = vm.separateBacking('ref');
      await Future<void>.delayed(Duration.zero);
      separation.gate.complete();
      await run;

      expect(vm.practice!.reference!.primaryClip!.startMs, 5000);
      expect(vm.practice!.accompaniment!.primaryClip!.startMs, 5000);
    });

    test('Cancel after the stems arrive keeps the backing as it was', () async {
      final separation = _GatedSeparation();
      final waveforms = _SignalingWaveforms();
      final reference = AudioTrack(
        id: 'ref',
        kind: AudioTrackKind.reference,
        displayName: 'Original',
        audioFilePath: '/pool/original.wav',
        durationMs: 1000,
        createdAt: DateTime(2026),
      );
      final vm = await pumpVm(
        separation: separation,
        reference: reference,
        waveforms: waveforms,
      );
      for (var i = 0; i < 400 && !vm.separationAvailable; i++) {
        await Future<void>.delayed(const Duration(milliseconds: 5));
      }
      final backing = vm.practice!.accompaniment;

      final run = vm.separateBacking('ref');
      await Future<void>.delayed(Duration.zero);
      separation.gate.complete(); // the stems are in
      await waveforms.reached.future; // the instrumental is being analyzed
      expect(vm.isSeparating, isTrue, reason: 'Cancel is still on screen');
      vm.cancelSeparation();
      waveforms.gate.complete();
      await run;

      expect(vm.practice!.accompaniment?.id, backing?.id);
      expect(vm.practice!.separationSource, isNull);
      expect(events, isNot(contains('save')));
      expect(vm.isSeparating, isFalse);
    });

    test(
      'once the backing is being saved, Cancel is no longer offered',
      () async {
        // Pressed during the commit save, Cancel was ignored: the overlay still
        // offered it, then the backing was replaced anyway.
        final separation = _GatedSeparation();
        final waveforms = _SignalingWaveforms();
        final vm = await pumpVm(
          separation: separation,
          reference: AudioTrack(
            id: 'ref',
            kind: AudioTrackKind.reference,
            displayName: 'Original',
            audioFilePath: '/pool/original.wav',
            durationMs: 1000,
            createdAt: DateTime(2026),
          ),
          waveforms: waveforms,
        );
        for (var i = 0; i < 400 && !vm.separationAvailable; i++) {
          await Future<void>.delayed(const Duration(milliseconds: 5));
        }
        repo.saveGate = Completer<void>();
        final run = vm.separateBacking('ref');
        await Future<void>.delayed(Duration.zero);
        expect(vm.canCancelSeparation, isTrue);
        separation.gate.complete();
        await waveforms.reached.future;
        waveforms.gate.complete();
        for (var i = 0; i < 200 && repo.inFlight == 0; i++) {
          await Future<void>.delayed(Duration.zero);
        }
        expect(vm.isSeparating, isTrue);
        expect(vm.canCancelSeparation, isFalse);
        final gate = repo.saveGate!;
        repo.saveGate = null;
        gate.complete();
        await run;
        expect(vm.practice!.accompaniment, isNotNull);
      },
    );

    test(
      'a separation whose save fails leaves the selection on the Original',
      () async {
        // The separation flips the backing flag silently while it saves; that
        // save's selection check judged the backing lane by the instrumental's
        // slot (empty here) and moved the selection off the Original on screen,
        // and the failure path put only the flag back.
        final separation = _GatedSeparation();
        final reference = AudioTrack(
          id: 'ref',
          kind: AudioTrackKind.reference,
          displayName: 'Original',
          audioFilePath: '/pool/original.wav',
          durationMs: 1000,
          createdAt: DateTime(2026),
        );
        final vm = await pumpVm(separation: separation, reference: reference);
        for (var i = 0; i < 400 && !vm.separationAvailable; i++) {
          await Future<void>.delayed(const Duration(milliseconds: 5));
        }
        vm.setPlayReference(true);
        vm.selectTrack('ref');
        final run = vm.separateBacking('ref');
        await Future<void>.delayed(Duration.zero);
        repo.failNextSave = true;
        separation.gate.complete();
        await run;
        expect(vm.mixer.playReference, isTrue);
        expect(vm.selectedTrackId, 'ref');
      },
    );

    test(
      'a lane picked while a failed separation saves stays picked',
      () async {
        final separation = _GatedSeparation();
        final reference = AudioTrack(
          id: 'ref',
          kind: AudioTrackKind.reference,
          displayName: 'Original',
          audioFilePath: '/pool/original.wav',
          durationMs: 1000,
          createdAt: DateTime(2026),
        );
        final vm = await pumpVm(separation: separation, reference: reference);
        for (var i = 0; i < 400 && !vm.separationAvailable; i++) {
          await Future<void>.delayed(const Duration(milliseconds: 5));
        }
        vm.setPlayReference(true);
        vm.selectTrack('ref');
        final run = vm.separateBacking('ref');
        await Future<void>.delayed(Duration.zero);
        repo.failNextSave = true;
        final saves = repo.saveNo;
        // The failed save's revert re-reads the store: park it there.
        final reread = Completer<void>();
        repo.loadGate = reread;
        separation.gate.complete();
        for (var i = 0; i < 200 && repo.saveNo == saves; i++) {
          await Future<void>.delayed(Duration.zero);
        }
        final take = vm.practice!.vocalTracks.first.id;
        vm.selectTrack(take); // the user picks a lane meanwhile
        repo.loadGate = null;
        reread.complete();
        await run;
        expect(vm.selectedTrackId, take);
      },
    );

    test('separating a backing that is NOT yet the reference keeps the '
        'lane\'s mixer state', () async {
      // The ordinary first separation: the full mix sits in the ACCOMPANIMENT
      // slot, so the swap has to rebuild it under the reference kind. Only the
      // kind changes — it is the same lane, and everything the user set on it
      // has to survive. Volume and the colour tint were carried; pan, mute,
      // solo and the insert chain were silently reset.
      final separation = _GatedSeparation();
      final vm = await pumpVm(separation: separation);
      for (var i = 0; i < 400 && !vm.separationAvailable; i++) {
        await Future<void>.delayed(const Duration(milliseconds: 5));
      }
      await vm.addAccompaniment('/src/full-mix.wav');
      final backingId = vm.practice!.accompaniment!.id;

      vm.mixer.setTrackVolumeLive(backingId, 0.42);
      vm.mixer.setTrackPanLive(backingId, -0.6);
      await vm.mixer.toggleMute(backingId);
      vm.mixer.setTrackFxLive(backingId, TrackFx.flat().copyWith(eqOn: true));
      await vm.setTrackColor(backingId, 0xFF00FF00);

      final run = vm.separateBacking(backingId);
      await Future<void>.delayed(Duration.zero);
      separation.gate.complete();
      await run;

      final ref = vm.practice!.reference!;
      expect(ref.id, backingId, reason: 'the same lane, re-kinded');
      expect(ref.volume, closeTo(0.42, 1e-9));
      expect(ref.pan, closeTo(-0.6, 1e-9));
      expect(ref.muted, isTrue);
      expect(ref.fx?.eqOn, isTrue);
      expect(ref.colorValue, 0xFF00FF00);
    });

    // Regression guard: the separation success path imported the instrumental
    // stem directly — skipping ensureStreamableWav — so the separated backing
    // (a 44.1 kHz sidecar WAV) bypassed the disk-streaming path and was fully
    // re-decoded into RAM at every project open, exactly the failure mode the
    // import discipline exists to prevent.
    test('the separated instrumental goes through the same import discipline '
        'as every other import (transcoded to a streamable WAV)', () async {
      final separation = _GatedSeparation()..gate.complete();
      final edits = _FakeEdits()..transcodeTo = '/edit/inst48k.wav';
      final reference = AudioTrack(
        id: 'ref',
        kind: AudioTrackKind.reference,
        displayName: 'Original',
        audioFilePath: '/pool/original.wav',
        durationMs: 1000,
        createdAt: DateTime(2026),
      );
      final vm = await pumpVm(
        separation: separation,
        reference: reference,
        edits: edits,
      );
      for (var i = 0; i < 400 && !vm.separationAvailable; i++) {
        await Future<void>.delayed(const Duration(milliseconds: 5));
      }

      await vm.separateBacking('ref');

      expect(edits.streamableCalls, ['/import/0.wav']);
      expect(
        vm.practice!.accompaniment!.clips.single.audioFilePath,
        '/edit/inst48k.wav',
        reason: 'the committed backing must be the streamable WAV',
      );
      expect(
        events,
        contains('delete:/import/0.wav'),
        reason: 'the non-streamable import copy is redundant once transcoded',
      );
    });

    test('a FAILED save after separation deletes the transcoded WAV and the '
        'import copy, and keeps nothing live', () async {
      final separation = _GatedSeparation()..gate.complete();
      final edits = _FakeEdits()..transcodeTo = '/edit/inst48k.wav';
      final reference = AudioTrack(
        id: 'ref',
        kind: AudioTrackKind.reference,
        displayName: 'Original',
        audioFilePath: '/pool/original.wav',
        durationMs: 1000,
        createdAt: DateTime(2026),
      );
      final vm = await pumpVm(
        separation: separation,
        reference: reference,
        edits: edits,
      );
      for (var i = 0; i < 400 && !vm.separationAvailable; i++) {
        await Future<void>.delayed(const Duration(milliseconds: 5));
      }
      repo.failSaves = true;

      await vm.separateBacking('ref');

      expect(events, contains('delete:/edit/inst48k.wav'));
      expect(events, contains('delete:/import/0.wav'));
      expect(vm.practice!.accompaniment, isNull);
    });

    test('a FAILED save after separation tells the engine the backing toggle '
        'came back', () async {
      // The flow flips the toggle to the instrumental silently before its
      // save, and the save's failure path resyncs the engine under that
      // flipped flag. Restoring the flag silently afterwards left the toggle
      // showing Original while the engine played the accompaniment side and
      // kept the original muted.
      final separation = _GatedSeparation()..gate.complete();
      final engine = _MuteTrackingEngine();
      final reference = AudioTrack(
        id: 'ref',
        kind: AudioTrackKind.reference,
        displayName: 'Original',
        audioFilePath: '/pool/original.wav',
        durationMs: 1000,
        createdAt: DateTime(2026),
      );
      final vm = await pumpVm(
        separation: separation,
        reference: reference,
        engine: engine,
      );
      for (var i = 0; i < 400 && !vm.separationAvailable; i++) {
        await Future<void>.delayed(const Duration(milliseconds: 5));
      }
      vm.mixer.setPlayReference(true); // listening to the original
      final refClip = vm.practice!.reference!.clips.single.id;
      expect(engine.muted[refClip], isFalse);
      repo.failSaves = true;

      await vm.separateBacking('ref');

      expect(vm.mixer.playReference, isTrue);
      expect(engine.muted[refClip], isFalse, reason: 'the original must play');
    });

    test('addVocalTake on a FAILED save deletes the imported file', () async {
      final vm = await pumpVm();
      repo.failSaves = true;
      final before = vm.practice!.vocalTracks.length;

      await vm.addVocalTake('/src/a.wav');

      expect(events, contains('delete:/import/0.wav'));
      expect(vm.practice!.vocalTracks.length, before);
    });

    test('addAccompaniment deletes the replaced backing only AFTER the save '
        'landed, and a FAILED save keeps it', () async {
      final vm = await pumpVm();
      await vm.addAccompaniment('/src/back1.wav'); // -> /import/0.wav
      final oldPath = vm.practice!.accompaniment!.clips.single.audioFilePath;
      events.clear();

      await vm.addAccompaniment('/src/back2.wav'); // replaces the first
      final saveIdx = events.indexOf('save');
      final delIdx = events.indexOf('delete:$oldPath');
      expect(saveIdx, isNonNegative);
      expect(
        delIdx,
        greaterThan(saveIdx),
        reason: 'the replaced backing falls only after the save landed',
      );

      final curPath = vm.practice!.accompaniment!.clips.single.audioFilePath;
      events.clear();
      repo.failSaves = true;
      await vm.addAccompaniment('/src/back3.wav');

      expect(
        events,
        isNot(contains('delete:$curPath')),
        reason: 'a failed replacement must keep the current backing audio',
      );
      expect(vm.practice!.accompaniment!.clips.single.audioFilePath, curPath);
      // The freshly imported file was rolled back.
      expect(events.where((e) => e.startsWith('delete:/import/')), isNotEmpty);
    });

    test('duplicateTrack on a FAILED save deletes the copied files', () async {
      final vm = await pumpVm();
      repo.failSaves = true;

      await vm.duplicateTrack('single');

      expect(events, contains('delete:/edit/dup0.wav'));
      expect(vm.practice!.vocalTracks.map((t) => t.id), ['single', 'multi']);
    });

    test(
      'a duplicate is of the take as it is once the files are copied',
      () async {
        // Built from the snapshot taken before the copies, it missed a rename
        // and a clip drag made while they ran.
        final edits = _FakeEdits()..duplicateGate = Completer<void>();
        final vm = await pumpVm(edits: edits);
        final dup = vm.duplicateTrack('multi');
        await Future<void>.delayed(Duration.zero);
        await vm.renameTrack('multi', 'Verse');
        vm.moveClipStartLive('multi', 'clip-b', 700);
        await vm.commitTrackMove();
        edits.duplicateGate!.complete();
        await dup;

        final copy = vm.practice!.vocalTracks.last;
        expect(copy.displayName, 'Verse copy');
        expect(copy.clips.map((c) => c.startMs), [0, 700]);
      },
    );

    test('duplicateTrack that fails PART WAY through a multi-clip lane '
        'deletes the copies that already landed', () async {
      // The cleanup path used to be declared only after the copy loop, so a
      // throw on the second clip (disk full) left the first copy orphaned in
      // edits/ with nothing referencing it until the next healthy boot swept
      // it up.
      final edits = _FakeEdits()..failDuplicateAt = 1;
      final vm = await pumpVm(edits: edits);

      await vm.duplicateTrack('multi');

      expect(events, contains('delete:/edit/dup0.wav'));
      expect(vm.practice!.vocalTracks.map((t) => t.id), ['single', 'multi']);
    });
  });

  group('remote-applied reload', () {
    test('a take that arrived from another device is inside the timeline and '
        'refreshes the analysis subject', () async {
      // The reload replaces the aggregate wholesale, exactly like the initial
      // load — so it owes the model the same normalizations. It used to skip
      // _ensureTimeline, leaving a remotely-added take sitting past the end of
      // the local timeline: invisible on the ruler and unreachable by the
      // transport until the next save happened to re-grow it.
      final remote = StreamController<RemoteApplied>.broadcast();
      addTearDown(remote.close);
      final vm = await pumpVm(remoteApplied: remote.stream);
      final localTimeline = vm.practice!.timelineDurationMs;
      expect(localTimeline, greaterThan(0));

      repo.stored = repo.stored.copyWith(
        vocalTracks: [
          ...repo.stored.vocalTracks,
          AudioTrack(
            id: 'from-peer',
            kind: AudioTrackKind.vocal,
            displayName: 'Peer take',
            audioFilePath: '/pool/peer.wav',
            durationMs: 5000,
            startMs: localTimeline + 60000,
            createdAt: DateTime(2026),
          ),
        ],
      );
      remote.add(
        RemoteApplied(
          rows: const [],
          practiceIds: const {'p1'},
          techniqueIds: const {},
        ),
      );
      for (var i = 0; i < 400 && vm.practice!.vocalTracks.length < 3; i++) {
        await Future<void>.delayed(const Duration(milliseconds: 5));
      }

      final p = vm.practice!;
      expect(p.vocalTracks.map((t) => t.id), contains('from-peer'));
      expect(
        p.timelineDurationMs,
        greaterThanOrEqualTo(p.contentEndMs),
        reason: 'the timeline must still cover every lane',
      );
    });

    test('a failed save keeps the timeline over every lane', () async {
      // A failed save reverts to the stored aggregate, whose timeline need not
      // cover its takes (one recovered or synced past it): grown on load, it
      // shrank back on the revert and cut the take off at the right edge.
      final vm = await pumpVm();
      repo.stored = repo.stored.copyWith(
        timelineDurationMs: 1500,
        vocalTracks: [
          ...repo.stored.vocalTracks,
          AudioTrack(
            id: 'late',
            kind: AudioTrackKind.vocal,
            displayName: 'Take 3 (recovered)',
            audioFilePath: '/pool/late.wav',
            durationMs: 5000,
            startMs: 60000,
            createdAt: DateTime(2026),
          ),
        ],
      );
      repo.failSaves = true;

      await vm.renameTrack('single', 'renamed');

      final p = vm.practice!;
      expect(p.vocalTracks.map((t) => t.id), contains('late'));
      expect(p.timelineDurationMs, greaterThanOrEqualTo(p.contentEndMs));
    });

    test('a live fader edit made while the reload reads survives it', () async {
      // Live edits (fader/pan ticks, clip drags, FX knobs) change the model
      // without saving or bumping the reload epoch, so a reload whose read
      // was in flight adopted the disk state over them: the slider snapped
      // back and the release committed the OLD value.
      final remote = StreamController<RemoteApplied>.broadcast();
      addTearDown(remote.close);
      final vm = await pumpVm(remoteApplied: remote.stream);

      repo.loadGate = Completer<void>();
      remote.add(
        RemoteApplied(
          rows: const [],
          practiceIds: const {'p1'},
          techniqueIds: const {},
        ),
      );
      await Future<void>.delayed(Duration.zero);
      vm.mixer.setTrackVolumeLive('single', 0.3); // dragging, not released
      repo.loadGate!.complete(); // the reload's read lands mid-drag
      repo.loadGate = null;
      await Future<void>.delayed(const Duration(milliseconds: 20));
      await vm.mixer.commitTrackVolume(); // release

      final single = repo.stored.vocalTracks.firstWhere(
        (t) => t.id == 'single',
      );
      expect(single.volume, 0.3);
    });

    test(
      'a remote change landing during the first load is loaded after it',
      () async {
        // Skipped while the page's first read was out, and never replayed: the
        // change stayed invisible until some later save re-read the store.
        final remote = StreamController<RemoteApplied>.broadcast();
        addTearDown(remote.close);
        final firstRead = Completer<void>();
        final vm = await pumpVm(
          remoteApplied: remote.stream,
          firstLoad: firstRead,
        );
        await Future<void>.delayed(Duration.zero);
        expect(vm.load.running, isTrue);
        repo.stored = repo.stored.copyWith(title: 'Renamed elsewhere');
        remote.add(
          RemoteApplied(
            rows: const [],
            practiceIds: const {'p1'},
            techniqueIds: const {},
          ),
        );
        await Future<void>.delayed(Duration.zero);
        firstRead.complete();
        for (
          var i = 0;
          i < 100 && vm.practice?.title != 'Renamed elsewhere';
          i++
        ) {
          await Future<void>.delayed(const Duration(milliseconds: 5));
        }
        expect(vm.practice!.title, 'Renamed elsewhere');
      },
    );

    test('a fader held still when a reload arrives keeps its value', () async {
      // The reload skipped only an edit made DURING its read: a value held
      // since before it (the fader still down, not moving) was read over with
      // the store's, the slider snapped back and the release saved the old
      // value.
      final remote = StreamController<RemoteApplied>.broadcast();
      addTearDown(remote.close);
      final vm = await pumpVm(remoteApplied: remote.stream);
      vm.mixer.setTrackVolumeLive('single', 0.3); // held, not released

      remote.add(
        RemoteApplied(
          rows: const [],
          practiceIds: const {'p1'},
          techniqueIds: const {},
        ),
      );
      await Future<void>.delayed(const Duration(milliseconds: 20));
      double volume(SongPractice p) =>
          p.vocalTracks.firstWhere((t) => t.id == 'single').volume;
      expect(volume(vm.practice!), 0.3);
      await vm.mixer.commitTrackVolume(); // release

      expect(volume(repo.stored), 0.3);
    });

    test('a reload after a failed save\'s revert shows the store', () async {
      final remote = StreamController<RemoteApplied>.broadcast();
      addTearDown(remote.close);
      final vm = await pumpVm(remoteApplied: remote.stream);
      repo.failAt = {repo.saveNo + 1};
      await vm.renameTrack('single', 'lost'); // fails; reverted to the store

      repo.stored = repo.stored.copyWith(title: 'Renamed elsewhere');
      remote.add(
        RemoteApplied(
          rows: const [],
          practiceIds: const {'p1'},
          techniqueIds: const {},
        ),
      );
      await Future<void>.delayed(const Duration(milliseconds: 20));

      expect(vm.practice!.title, 'Renamed elsewhere');
    });

    test('... and a reload after the release shows the store', () async {
      final remote = StreamController<RemoteApplied>.broadcast();
      addTearDown(remote.close);
      final vm = await pumpVm(remoteApplied: remote.stream);
      vm.mixer.setTrackVolumeLive('single', 0.3);
      await vm.mixer.commitTrackVolume();

      repo.stored = repo.stored.copyWith(title: 'Renamed elsewhere');
      remote.add(
        RemoteApplied(
          rows: const [],
          practiceIds: const {'p1'},
          techniqueIds: const {},
        ),
      );
      await Future<void>.delayed(const Duration(milliseconds: 20));

      expect(vm.practice!.title, 'Renamed elsewhere');
    });
  });

  group('take numbering', () {
    // Regression guard: boot recovery numbers takes with a regex that counts
    // "Take N (recovered)" names, but the view model's regex did not — so the
    // take recorded after a recovery reused the recovered take's number and
    // the lane list showed two takes with the same number.
    test('a new take numbers PAST a recovered take instead of duplicating '
        'its number', () async {
      final vm = await pumpVm(
        vocalTracks: [
          AudioTrack(
            id: 't1',
            kind: AudioTrackKind.vocal,
            displayName: 'Take 1',
            createdAt: DateTime(2026),
          ),
          AudioTrack(
            id: 't3r',
            kind: AudioTrackKind.vocal,
            displayName: 'Take 3 (recovered)',
            createdAt: DateTime(2026),
          ),
        ],
      );

      await vm.createEmptyVocalTake();

      expect(vm.practice!.vocalTracks.last.displayName, 'Take 4');
    });
  });

  group('save serialization', () {
    test('two rapid ungated saves never overlap (second chains after the '
        'first and the last write wins)', () async {
      final vm = await pumpVm();
      final f1 = vm.renameTrack('single', 'A');
      final f2 = vm.renameTrack('single', 'B');
      await Future.wait([f1, f2]);

      expect(repo.maxInFlight, 1, reason: 'saves must be serialized');
      expect(
        repo.stored.vocalTracks.firstWhere((t) => t.id == 'single').displayName,
        'B',
      );
    });

    test('an edit made WHILE a save is failing in flight survives (the '
        'failure revert must not clobber the newer queued model)', () async {
      // Regression: edit A's save is in flight (its model snapshot already
      // captured) when edit B mutates the live model and queues save B. Save
      // A then fails; its revert reloads disk state — without the
      // identical(_practice, sent) guard it would overwrite B's model, so
      // save B would persist PRE-EDIT state: edit B silently lost on disk
      // with only A's failure toast shown. (When BOTH edits land before the
      // save captures its snapshot, the failed write genuinely contained
      // both and the revert-with-toast is the designed failure UX — that
      // case is not what this test pins.)
      final vm = await pumpVm();
      final toasts = <String>[];
      vm.toasts.listen(toasts.add);
      repo.failNextSave = true;

      final f1 = vm.renameTrack('single', 'A'); // this save fails
      // Yield one microtask so save A starts and captures its model snapshot
      // (it is now suspended inside the repository write) — THEN edit B
      // arrives, exactly the mid-flight window the guard protects.
      await null;
      final f2 = vm.renameTrack('single', 'B'); // queued behind, must survive
      await Future.wait([f1, f2]);
      await Future<void>.delayed(Duration.zero);

      expect(events, ['save-failed', 'save']);
      expect(
        repo.stored.vocalTracks.firstWhere((t) => t.id == 'single').displayName,
        'B',
        reason: "edit B must reach the store despite edit A's failed save",
      );
      expect(
        vm.practice!.vocalTracks
            .firstWhere((t) => t.id == 'single')
            .displayName,
        'B',
        reason: 'the screen must keep showing the surviving edit',
      );
      expect(toasts, contains("Couldn't save your changes."));
    });
  });

  group('a failed save that could not revert', () {
    // With no store read to revert to, the failed change stays on screen, a
    // remote reload holds off for it, and the next save writes it with its
    // own.
    RemoteApplied event() => RemoteApplied(
      rows: const [],
      practiceIds: const {'p1'},
      techniqueIds: const {},
    );

    test('is kept through a reload and retried by the next save', () async {
      final remote = StreamController<RemoteApplied>.broadcast();
      addTearDown(remote.close);
      final vm = await pumpVm(remoteApplied: remote.stream);
      repo.failAt = {repo.saveNo + 1};
      repo.failNextLoad = true;
      await vm.renameTrack('single', 'kept');
      repo.stored = repo.stored.copyWith(title: 'Renamed elsewhere');
      remote.add(event());
      await Future<void>.delayed(const Duration(milliseconds: 20));
      String name(SongPractice p, String id) =>
          p.vocalTracks.firstWhere((t) => t.id == id).displayName;
      expect(name(vm.practice!, 'single'), 'kept');

      await vm.renameTrack('multi', 'next');

      expect(name(repo.stored, 'single'), 'kept');
      expect(name(repo.stored, 'multi'), 'next');
    });

    test('a lyric stamp is kept for the next tap', () async {
      // Replaced by a later reload, the failed stamp went without the sync
      // cursor moving back, and the next tap stamped the line after it.
      final remote = StreamController<RemoteApplied>.broadcast();
      addTearDown(remote.close);
      final vm = await pumpVm(remoteApplied: remote.stream);
      await vm.setLyricsText('a\nb\nc\nd');
      Future<void> tapAt(int ms) async {
        vm.playback.position.value = Duration(milliseconds: ms);
        await vm.stampNextLyric();
      }

      await tapAt(1000);
      repo.failAt = {repo.saveNo + 1};
      repo.failNextLoad = true;
      await tapAt(2000);
      remote.add(event());
      await Future<void>.delayed(const Duration(milliseconds: 20));
      await tapAt(3000);

      expect(repo.stored.lyrics.map((l) => l.timeMs), [1000, 2000, 3000, null]);
    });

    test('... and an import it could not revert keeps its file', () async {
      // The import's rollback deleted the file the lane still on screen
      // played: the next save stored a lane whose audio was gone.
      final vm = await pumpVm();
      repo.failAt = {repo.saveNo + 1};
      repo.failNextLoad = true;
      await vm.addAccompaniment('/src/backing.wav');
      final path = vm.practice!.accompaniment!.primaryClip!.audioFilePath;
      expect(events, isNot(contains('delete:$path')));

      await vm.renameTrack('single', 'next');

      expect(repo.stored.accompaniment!.primaryClip!.audioFilePath, path);
    });

    test(
      '... while an import a revert took back still drops its file',
      () async {
        final vm = await pumpVm();
        repo.failAt = {repo.saveNo + 1};
        await vm.addAccompaniment('/src/backing.wav');

        expect(vm.practice!.accompaniment, isNull);
        expect(events, contains('delete:/import/0.wav'));
      },
    );
  });

  group('a save failing beside another', () {
    AudioTrack originalRef() => AudioTrack(
      id: 'ref',
      kind: AudioTrackKind.reference,
      displayName: 'Original',
      audioFilePath: '/pool/original.wav',
      durationMs: 1000,
      createdAt: DateTime(2026),
    );

    /// Runs [separate] with its save held, makes [edit] while it is held,
    /// then lets the save fail.
    Future<void> failSeparationAround(
      SongPracticeDetailViewModel vm,
      _GatedSeparation separation,
      Future<void> Function() separate,
      Future<void> Function() edit,
    ) async {
      for (var i = 0; i < 400 && !vm.separationAvailable; i++) {
        await Future<void>.delayed(const Duration(milliseconds: 5));
      }
      repo.saveGate = Completer<void>();
      repo.failAt = {repo.saveNo + 1};
      final separating = separate();
      separation.gate.complete();
      for (var i = 0; i < 400 && repo.inFlight == 0; i++) {
        await Future<void>.delayed(const Duration(milliseconds: 5));
      }
      expect(vm.practice!.accompaniment, isNotNull, reason: 'its save runs');
      final editing = edit();
      final gate = repo.saveGate!;
      repo.saveGate = null;
      gate.complete();
      await Future.wait([editing, separating]);
    }

    test('a failed separation keeps an edit made while its save ran', () async {
      // The failure path put back the whole model from before the separation,
      // dropping a rename made while its save ran. Taken back after that save,
      // the rename's save queued behind it had already sent the separation.
      final separation = _GatedSeparation();
      final vm = await pumpVm(separation: separation, reference: originalRef());
      await failSeparationAround(
        vm,
        separation,
        () => vm.separateBacking('ref'),
        () => vm.renameTrack('single', 'renamed'),
      );

      String name(SongPractice p) =>
          p.vocalTracks.firstWhere((t) => t.id == 'single').displayName;
      expect(vm.practice!.accompaniment, isNull);
      expect(name(vm.practice!), 'renamed');
      expect(repo.stored.accompaniment, isNull);
      expect(repo.stored.separationSource, isNull);
      expect(name(repo.stored), 'renamed');
    });
    test('... and an edit to the Original lane itself', () async {
      final separation = _GatedSeparation();
      final vm = await pumpVm(separation: separation, reference: originalRef());
      await failSeparationAround(
        vm,
        separation,
        () => vm.separateBacking('ref'),
        () => vm.renameTrack('ref', 'My original'),
      );

      expect(vm.practice!.reference!.displayName, 'My original');
      expect(repo.stored.reference!.displayName, 'My original');
      expect(repo.stored.accompaniment, isNull);
    });

    test(
      '... and puts a moved Original back in the backing slot, edited',
      () async {
        // The ordinary first separation moves the full mix from the backing
        // slot to the reference one: taken back, it returns there.
        final separation = _GatedSeparation();
        final vm = await pumpVm(separation: separation);
        await vm.addAccompaniment('/src/full-mix.wav');
        final backingId = vm.practice!.accompaniment!.id;
        await failSeparationAround(
          vm,
          separation,
          () => vm.separateBacking(backingId),
          () => vm.renameTrack(backingId, 'Full mix'),
        );

        for (final p in [vm.practice!, repo.stored]) {
          expect(p.reference, isNull);
          expect(p.accompaniment!.id, backingId);
          expect(p.accompaniment!.kind, AudioTrackKind.accompaniment);
          expect(p.accompaniment!.displayName, 'Full mix');
        }
      },
    );

    test('a taken-back separation leaves the engine without it', () async {
      // With no revert (the reload failed too), the take-back changed the
      // lanes after the failed save's resync: the engine kept the
      // instrumental, whose file the rollback then deleted.
      final separation = _GatedSeparation();
      final engine = _SyncRecordingEngine();
      final vm = await pumpVm(
        separation: separation,
        reference: originalRef(),
        engine: engine,
      );
      for (var i = 0; i < 400 && !vm.separationAvailable; i++) {
        await Future<void>.delayed(const Duration(milliseconds: 5));
      }
      repo.failAt = {repo.saveNo + 1};
      repo.failNextLoad = true;
      final separating = vm.separateBacking('ref');
      separation.gate.complete();
      await separating;

      expect(vm.practice!.accompaniment, isNull);
      expect(engine.lastPaths, isNot(contains('/import/0.wav')));
    });

    test('a lyrics Save reports it did not land', () async {
      final vm = await pumpVm();
      repo.failAt = {1};
      final first = vm.setLyricsText('new words');
      final second = vm.setLyricsText('new words');

      expect(await first, isFalse);
      expect(await second, isFalse, reason: 'the editor must keep the draft');
      expect(vm.lyrics.map((l) => l.text), ['la la la']);
      expect(repo.stored.lyrics.map((l) => l.text), ['la la la']);
    });
  });

  group('lyrics save failure', () {
    test(
      'a second Save of the same text waits for the write it repeats',
      () async {
        // The unchanged-text shortcut read the text already on screen, so a
        // second Save while the first write was queued said "saved" at once:
        // the editor closed, the write then failed and reverted the lines, and
        // the draft was gone.
        final vm = await pumpVm();
        repo.failSaves = true;

        final first = vm.setLyricsText('new words');
        final second = vm.setLyricsText('new words');

        expect(await first, isFalse);
        expect(await second, isFalse, reason: 'the editor must keep the draft');
        expect(vm.lyrics.map((l) => l.text), ['la la la']);
      },
    );

    test(
      'an unchanged Save after a revert that kept the text writes nothing',
      () async {
        // The lyrics write landed while a rename was queued (so it could not
        // clear the pending mark), the rename's save then failed, and its revert
        // found the lines unchanged and returned before clearing the mark: the
        // next unchanged Save wrote again, and with saves failing toasted a
        // failure over text already on disk.
        final vm = await pumpVm();
        repo.failAt = {2};
        final lyrics = vm.setLyricsText('new words');
        for (var i = 0; i < 50 && repo.inFlight == 0; i++) {
          await Future<void>.delayed(Duration.zero);
        }
        expect(repo.inFlight, 1, reason: 'the lyrics write is in flight');
        final rename = vm.renameTrack('single', 'renamed');
        expect(await lyrics, isTrue);
        await rename;
        expect(vm.lyrics.map((l) => l.text), ['new words']);

        repo.failSaves = true;
        final saves = repo.saveNo;
        expect(await vm.setLyricsText('new words'), isTrue);
        expect(repo.saveNo, saves);
      },
    );

    test(
      'a failed lyrics save reverts the optimistic lines and toasts',
      () async {
        final vm = await pumpVm();
        final toasts = <String>[];
        vm.toasts.listen(toasts.add);
        repo.failSaves = true;

        await vm.stampLyricAt(0); // optimistic timeMs stamp, then failed write
        await Future<void>.delayed(Duration.zero);

        expect(
          vm.practice!.lyrics.single.timeMs,
          isNull,
          reason: 'the un-persisted stamp must not stay on screen',
        );
        expect(toasts, contains("Couldn't save your changes."));
      },
    );

    // Regression guard: the cursor advance was computed AFTER the awaited
    // write, off the shared field, so two taps inside one save round-trip both
    // stamped the same line and the cursor then jumped over the next one.
    test(
      'two taps inside one save round-trip stamp two different lines',
      () async {
        final vm = await pumpVm();
        await vm.setLyricsText('one\ntwo\nthree');
        vm.playback.position.value = const Duration(milliseconds: 500);
        expect(vm.syncIndex, 0);

        await Future.wait([vm.stampNextLyric(), vm.stampNextLyric()]);

        expect(vm.lyrics[0].timeMs, isNotNull);
        expect(
          vm.lyrics[1].timeMs,
          isNotNull,
          reason:
              'the second tap must stamp the NEXT line, not re-stamp the first',
        );
        expect(vm.lyrics[2].timeMs, isNull);
        expect(vm.syncIndex, 2);
      },
    );

    // Regression guard: the editor routed on "is there a time tag anywhere",
    // and parseLrc drops untimed lines by design — so one bracketed timestamp
    // typed into ordinary lyrics deleted every other line on Save.
    test('a stray timestamp in typed lyrics keeps every line', () async {
      final vm = await pumpVm();

      final ok = await vm.applyEditedLyrics(
        'I waited [04:33] for you\nand then you came\nand we sang',
      );

      expect(ok, isTrue);
      expect(vm.lyrics.map((l) => l.text).toList(), [
        'I waited [04:33] for you',
        'and then you came',
        'and we sang',
      ]);
    });

    test(
      'pasting a real LRC document into the editor still syncs it',
      () async {
        final vm = await pumpVm();

        await vm.applyEditedLyrics(
          '[ti:Song]\n[00:12.50]hello\n[01:03.25]world',
        );

        expect(vm.lyrics.map((l) => l.text).toList(), ['hello', 'world']);
        expect(vm.lyrics.first.timeMs, 12500);
      },
    );

    test(
      'a failed stamp leaves the cursor on the line it could not stamp',
      () async {
        final vm = await pumpVm();
        await vm.setLyricsText('one\ntwo\nthree');
        repo.failSaves = true;

        await vm.stampNextLyric();

        expect(vm.lyrics[0].timeMs, isNull);
        expect(
          vm.syncIndex,
          0,
          reason: 'advancing past a line the write lost would skip it silently',
        );
      },
    );
  });

  group('lyrics sync cursor', () {
    test('a leading blank line never takes a tap', () async {
      // Pasted lyrics often open on a blank line, which the Sync view draws as
      // a gap with no highlight. After Clear (or re-entering Sync with every
      // line timed) the cursor sat on it, and every stamp landed a line late.
      final vm = await pumpVm();
      await vm.setLyricsText('\nfirst line\nsecond line');
      vm.setLyricsMode(LyricsMode.sync);
      await vm.clearLyricTimes();
      vm.playback.position.value = const Duration(milliseconds: 1500);
      await vm.stampNextLyric();
      expect(vm.lyrics[0].timeMs, isNull);
      expect(vm.lyrics[1].timeMs, 1500);

      vm.playback.position.value = const Duration(milliseconds: 2500);
      await vm.stampNextLyric();
      expect(vm.lyrics[2].timeMs, 2500);
      // Every line timed: Sync starts over from the first words.
      vm.setLyricsMode(LyricsMode.read);
      vm.setLyricsMode(LyricsMode.sync);
      expect(vm.syncIndex, 1);
    });

    test(
      're-timing a line by click, then Space, stamps the next line',
      () async {
        final vm = await pumpVm();
        await vm.setLyricsText('one\n\ntwo\nthree');
        vm.setLyricsMode(LyricsMode.sync);
        vm.playback.position.value = const Duration(milliseconds: 4000);
        await vm.stampLyricAt(2);
        vm.playback.position.value = const Duration(milliseconds: 6000);
        await vm.stampNextLyric();
        expect(
          vm.lyrics[2].timeMs,
          4000,
          reason: 'the click time was overwritten',
        );
        expect(vm.lyrics[3].timeMs, 6000);
      },
    );

    test('a click whose write fails leaves the cursor on that line', () async {
      final vm = await pumpVm();
      await vm.setLyricsText('one\ntwo\nthree');
      vm.setLyricsMode(LyricsMode.sync);
      repo.failSaves = true;
      await vm.stampLyricAt(1);
      expect(vm.lyrics[1].timeMs, isNull);
      expect(vm.syncIndex, 1);
    });

    test('a text file with one bracketed time keeps every line', () async {
      // Routed as LRC on a single time tag, a .txt kept only that line:
      // parseLrc drops untimed lines, and the import replaced the lyrics.
      final vm = await pumpVm();
      final dir = await Directory.systemTemp.createTemp('vocara_lyrics');
      addTearDown(() => dir.delete(recursive: true));
      final file = File('${dir.path}/song.txt')
        ..writeAsStringSync('Verse one\nVerse two\nGuitar solo [2:30]\nOutro');
      await vm.importLyricsFromFile(file.path);
      expect(vm.lyrics.map((l) => l.text), [
        'Verse one',
        'Verse two',
        'Guitar solo [2:30]',
        'Outro',
      ]);
    });

    test('lyrics dropped in Sync mode restart the cursor', () async {
      final vm = await pumpVm();
      await vm.setLyricsText('a\nb\nc\nd\ne\nf');
      vm.setLyricsMode(LyricsMode.sync);
      vm.playback.position.value = const Duration(milliseconds: 1000);
      for (var i = 0; i < 5; i++) {
        await vm.stampNextLyric();
      }
      expect(vm.syncIndex, 5);
      await vm.importLyrics('x\ny');
      expect(vm.syncIndex, 0);
      vm.playback.position.value = const Duration(milliseconds: 2000);
      await vm.stampNextLyric();
      expect(vm.lyrics[0].timeMs, 2000, reason: 'Space did nothing');
    });

    test(
      'new words from another device restart the cursor; new times do not',
      () async {
        final remote = StreamController<RemoteApplied>.broadcast();
        addTearDown(remote.close);
        final vm = await pumpVm(remoteApplied: remote.stream);
        await vm.setLyricsText('a\nb\nc\nd');
        vm.setLyricsMode(LyricsMode.sync);
        vm.playback.position.value = const Duration(milliseconds: 1000);
        await vm.stampNextLyric();
        await vm.stampNextLyric();
        expect(vm.syncIndex, 2);

        Future<void> reloadWith(List<LyricLine> lines) async {
          repo.stored = repo.stored.copyWith(lyrics: lines);
          remote.add(
            RemoteApplied(
              rows: const [],
              practiceIds: const {'p1'},
              techniqueIds: const {},
            ),
          );
          for (var i = 0; i < 400 && !_sameLyrics(vm.lyrics, lines); i++) {
            await Future<void>.delayed(const Duration(milliseconds: 5));
          }
          expect(_sameLyrics(vm.lyrics, lines), isTrue);
        }

        // A peer stamped line d: the same words, so the cursor stays on c.
        await reloadWith([
          ...vm.lyrics.take(3),
          vm.lyrics[3].copyWith(timeMs: 9000),
        ]);
        expect(vm.syncIndex, 2);
        // A peer cleared every time: start over on the first words, as a clear
        // here does — left on c, the next tap timed c with a and b untimed.
        await reloadWith([
          for (final l in vm.lyrics) l.copyWith(clearTime: true),
        ]);
        expect(vm.syncIndex, 0);
        vm.playback.position.value = const Duration(milliseconds: 1500);
        await vm.stampNextLyric();
        expect(vm.lyrics[0].timeMs, 1500);
        expect(vm.syncIndex, 1);
        // A peer replaced the words: the cursor's place in the old ones means nothing.
        await reloadWith(const [LyricLine(text: 'x'), LyricLine(text: 'y')]);
        expect(vm.syncIndex, 0);
      },
    );
  });

  group('editing timed lyrics', () {
    // Times were matched to the edited text by line text alone, first come
    // first served, and a plain-text round trip dropped the times on blank
    // lines (end-of-line and instrumental markers).
    test(
      'a fixed typo in the first chorus leaves the second its own time',
      () async {
        final vm = await pumpVm();
        expect(
          await vm.importLyrics(
            '[00:30.00]chorus\n[00:40.00]verse\n[01:30.00]chorus',
          ),
          isTrue,
        );
        await vm.setLyricsText('chorus!\nverse\nchorus');
        expect(vm.lyrics.map((l) => l.timeMs), [null, 40000, 90000]);
        await vm.setLyricsText('verse\nchorus'); // the first chorus deleted
        expect(vm.lyrics.map((l) => l.timeMs), [40000, 90000]);
      },
    );

    test('an unchanged save keeps the timed blank lines', () async {
      final vm = await pumpVm();
      await vm.importLyrics(
        '[00:10.00]hello\n[00:15.00]\n[00:20.00]world\n[00:25.00]',
      );
      final before = [for (final l in vm.lyrics) (l.text, l.timeMs)];
      expect(before, [
        ('hello', 10000),
        ('', 15000),
        ('world', 20000),
        ('', 25000),
      ]);
      expect(await vm.setLyricsText(vm.lyricsAsText()), isTrue);
      expect([for (final l in vm.lyrics) (l.text, l.timeMs)], before);
    });

    test('an edit keeps the end marker of a line that is still last', () async {
      final vm = await pumpVm();
      await vm.importLyrics(
        '[00:10.00]hello\n[00:15.00]\n[00:20.00]world\n[00:25.00]',
      );
      await vm.setLyricsText('hello there\n\nworld');
      expect(
        [for (final l in vm.lyrics) (l.text, l.timeMs)],
        [('hello there', null), ('', 15000), ('world', 20000), ('', 25000)],
      );
    });
  });

  group('the sync cursor follows the words under it', () {
    test(
      'the same words re-imported without their times restart the cursor',
      () async {
        final vm = await pumpVm();
        await vm.setLyricsText('a\nb\nc\nd\ne\nf');
        vm.setLyricsMode(LyricsMode.sync);
        vm.playback.position.value = const Duration(milliseconds: 1000);
        for (var i = 0; i < 3; i++) {
          await vm.stampNextLyric();
        }
        expect(vm.syncIndex, 3);
        // In Sync mode the user drops the same plain lyrics again: every time is gone.
        expect(await vm.importLyrics('a\nb\nc\nd\ne\nf'), isTrue);
        expect(vm.lyrics.every((l) => l.timeMs == null), isTrue);
        vm.playback.position.value = const Duration(milliseconds: 5000);
        await vm.stampNextLyric();
        expect(
          vm.lyrics[0].timeMs,
          5000,
          reason: 'the cursor stayed on d while a-c had lost their times',
        );
      },
    );

    test(
      'a failed same-words re-import read mid-save restores the cursor',
      () async {
        final vm = await pumpVm();
        await vm.setLyricsText('a\nb\nc\nd\ne\nf');
        vm.setLyricsMode(LyricsMode.sync);
        vm.playback.position.value = const Duration(milliseconds: 1000);
        for (var i = 0; i < 3; i++) {
          await vm.stampNextLyric();
        }
        expect(vm.syncIndex, 3);
        repo.failSaves = true;
        final landed = vm.importLyrics('a\nb\nc\nd\ne\nf');
        // The import's notify rebuilds the Sync view on the next frame, which
        // reads the cursor while the write is still in flight.
        expect(vm.syncIndex, 0);
        expect(await landed, isFalse);
        repo.failSaves = false;
        expect(vm.lyrics.take(3).every((l) => l.timeMs == 1000), isTrue);
        vm.playback.position.value = const Duration(milliseconds: 7000);
        await vm.stampNextLyric();
        expect(vm.lyrics[0].timeMs, 1000, reason: 'Space re-stamped line a');
        expect(vm.lyrics[3].timeMs, 7000, reason: 'd never got the tap');
      },
    );

    test(
      'a tap landed while a failed import was in flight keeps its cursor',
      () async {
        final vm = await pumpVm();
        await vm.setLyricsText('a\nb\nc\nd');
        vm.setLyricsMode(LyricsMode.sync);
        // A transient failure: the import's write fails, the tap's lands.
        repo.failNextSave = true;
        final landed = vm.importLyrics(
          '[00:01.00]a\n[00:02.00]b\n[00:03.00]c\n[00:04.00]d',
        );
        // Every line timed: the cursor starts over on the first words.
        expect(vm.syncIndex, 0);
        await Future<void>.delayed(Duration.zero); // the write is in flight
        // The user re-times from the top while the import's write is in flight.
        vm.playback.position.value = const Duration(milliseconds: 1500);
        final tap = vm.stampNextLyric();
        expect(vm.syncIndex, 1);
        expect(await landed, isFalse);
        await tap;
        expect(vm.lyrics[0].timeMs, 1500);
        expect(
          vm.syncIndex,
          1,
          reason: 'the failed import re-seeded over the tap',
        );
        vm.playback.position.value = const Duration(milliseconds: 2500);
        await vm.stampNextLyric();
        expect(vm.lyrics[0].timeMs, 1500, reason: 'Space re-stamped line a');
        expect(vm.lyrics[1].timeMs, 2500, reason: 'b never got the tap');
      },
    );

    test(
      'a click landed while a failed import was in flight keeps its cursor',
      () async {
        final vm = await pumpVm();
        await vm.setLyricsText('a\nb\nc\nd\ne');
        vm.setLyricsMode(LyricsMode.sync);
        repo.failNextSave = true;
        final landed = vm.importLyrics('a\nb\nc\nd\ne');
        expect(vm.syncIndex, 0);
        await Future<void>.delayed(Duration.zero); // the write is in flight
        vm.playback.position.value = const Duration(milliseconds: 3000);
        final click = vm.stampLyricAt(2);
        expect(vm.syncIndex, 3);
        expect(await landed, isFalse);
        await click;
        expect(
          vm.syncIndex,
          3,
          reason: 'the failed import re-seeded over the click',
        );
      },
    );

    test(
      'a failed re-import and a failed tap never put the cursor on a timed line',
      () async {
        final vm = await pumpVm();
        await vm.setLyricsText('a\nb\nc\nd\ne\nf');
        vm.setLyricsMode(LyricsMode.sync);
        vm.playback.position.value = const Duration(milliseconds: 1000);
        for (var i = 0; i < 3; i++) {
          await vm.stampNextLyric();
        }
        expect(vm.syncIndex, 3);
        repo.failSaves = true;
        final landed = vm.importLyrics('a\nb\nc\nd\ne\nf');
        expect(vm.syncIndex, 0); // a frame mid-write
        await Future<void>.delayed(Duration.zero); // the write is in flight
        vm.playback.position.value = const Duration(milliseconds: 2000);
        final tap = vm.stampNextLyric(); // Space while the import is in flight
        expect(await landed, isFalse);
        vm.syncIndex; // a frame drawn after the import's failure
        await tap;
        repo.failSaves = false;
        expect(vm.lyrics.take(3).every((l) => l.timeMs == 1000), isTrue);
        vm.playback.position.value = const Duration(milliseconds: 7000);
        await vm.stampNextLyric();
        expect(vm.lyrics[0].timeMs, 1000, reason: 'Space re-stamped line a');
        expect(vm.lyrics[3].timeMs, 7000, reason: 'd never got the tap');
      },
    );

    test('the same, with no frame drawn between the two failures', () async {
      final vm = await pumpVm();
      await vm.setLyricsText('a\nb\nc\nd\ne\nf');
      vm.setLyricsMode(LyricsMode.sync);
      vm.playback.position.value = const Duration(milliseconds: 1000);
      for (var i = 0; i < 3; i++) {
        await vm.stampNextLyric();
      }
      repo.failSaves = true;
      final landed = vm.importLyrics('a\nb\nc\nd\ne\nf');
      expect(vm.syncIndex, 0);
      await Future<void>.delayed(Duration.zero); // the write is in flight
      vm.playback.position.value = const Duration(milliseconds: 2000);
      final tap = vm.stampNextLyric();
      expect(await landed, isFalse);
      await tap;
      repo.failSaves = false;
      vm.playback.position.value = const Duration(milliseconds: 7000);
      await vm.stampNextLyric();
      expect(vm.lyrics[0].timeMs, 1000, reason: 'Space re-stamped line a');
      expect(vm.lyrics[3].timeMs, 7000, reason: 'd never got the tap');
    });

    test(
      'a failed import under a newer one leaves the newer cursor alone',
      () async {
        final vm = await pumpVm();
        await vm.setLyricsText('a\nb\nc');
        vm.setLyricsMode(LyricsMode.sync);
        repo.failNextSave = true;
        final first = vm.importLyrics('x\ny\nz');
        await Future<void>.delayed(Duration.zero);
        final second = vm.importLyrics('p\nq\nr');
        expect(vm.syncIndex, 0);
        vm.playback.position.value = const Duration(milliseconds: 1000);
        final tap = vm.stampLyricAt(1);
        expect(vm.syncIndex, 2);
        expect(await first, isFalse);
        expect(await second, isTrue);
        await tap;
        expect(vm.lyrics.map((l) => '${l.text}@${l.timeMs}').toList(), [
          'p@null',
          'q@1000',
          'r@null',
        ]);
        expect(vm.syncIndex, 2);
      },
    );

    test(
      'an edit and an import both failing leave the cursor on the restored text',
      () async {
        final vm = await pumpVm();
        await vm.setLyricsText('a\nb\nc');
        vm.setLyricsMode(LyricsMode.sync);
        vm.playback.position.value = const Duration(milliseconds: 1000);
        await vm.stampNextLyric();
        expect(vm.syncIndex, 1);
        repo.failSaves = true;
        final edit = vm.applyEditedLyrics('a\nb\nc\nd');
        final imp = vm.importLyrics('x\ny');
        expect(vm.syncIndex, 0);
        expect(await edit, isFalse);
        expect(await imp, isFalse);
        repo.failSaves = false;
        expect(vm.lyrics.map((l) => l.text).toList(), ['a', 'b', 'c']);
        expect(vm.syncIndex, 1);
      },
    );

    test(
      'a failed import puts the cursor back on the restored words',
      () async {
        final vm = await pumpVm();
        await vm.setLyricsText('a\nb\nc\nd\ne\nf');
        vm.setLyricsMode(LyricsMode.sync);
        vm.playback.position.value = const Duration(milliseconds: 1000);
        for (var i = 0; i < 5; i++) {
          await vm.stampNextLyric();
        }
        expect(vm.syncIndex, 5);
        repo.failSaves = true;
        expect(await vm.importLyrics('x\ny'), isFalse);
        repo.failSaves = false;
        expect(vm.lyrics.map((l) => l.text).toList(), [
          'a',
          'b',
          'c',
          'd',
          'e',
          'f',
        ]);
        vm.playback.position.value = const Duration(milliseconds: 7000);
        await vm.stampNextLyric();
        // The restored words are timed through e: the tap belongs on f.
        expect(vm.lyrics[0].timeMs, 1000, reason: 'Space re-stamped line a');
        expect(vm.lyrics[5].timeMs, 7000, reason: 'f never got the tap');
      },
    );

    test('a failed import never leaves the cursor past the end', () async {
      final vm = await pumpVm();
      await vm.setLyricsText('a');
      vm.setLyricsMode(LyricsMode.sync);
      expect(vm.syncIndex, 0);
      repo.failSaves = true;
      expect(await vm.importLyrics('\n\nx'), isFalse);
      repo.failSaves = false;
      expect(vm.lyrics.map((l) => l.text).toList(), ['a']);
      vm.playback.position.value = const Duration(milliseconds: 3000);
      await vm.stampNextLyric();
      expect(vm.syncIndex, lessThanOrEqualTo(vm.lyrics.length));
      expect(vm.lyrics[0].timeMs, 3000, reason: 'Space did nothing');
    });

    test('peer words merged into an ordinary save re-seed the cursor', () async {
      final vm = await pumpVm();
      await vm.setLyricsText('a\nb\nc\nd\ne\nf');
      vm.setLyricsMode(LyricsMode.sync);
      vm.playback.position.value = const Duration(milliseconds: 1000);
      for (var i = 0; i < 5; i++) {
        await vm.stampNextLyric();
      }
      expect(vm.syncIndex, 5);
      repo.peerWordsOnNextSave = const [
        LyricLine(text: 'x'),
        LyricLine(text: 'y'),
      ];
      await vm.commitTrackMove(); // any non-lyrics save (a drag released)
      expect(vm.lyrics.map((l) => l.text).toList(), ['x', 'y']);
      vm.playback.position.value = const Duration(milliseconds: 2000);
      await vm.stampNextLyric();
      expect(
        vm.lyrics[0].timeMs,
        2000,
        reason:
            'Space did nothing: cursor ${vm.syncIndex} of ${vm.lyrics.length}',
      );
    });

    test(
      'a failed stamp reverting onto a peer\'s words re-seeds the cursor',
      () async {
        final vm = await pumpVm();
        await vm.setLyricsText('a\nb\nc\nd\ne\nf');
        vm.setLyricsMode(LyricsMode.sync);
        vm.playback.position.value = const Duration(milliseconds: 1000);
        for (var i = 0; i < 5; i++) {
          await vm.stampNextLyric();
        }
        // A peer's new words reached the disk; this tap's write fails and the model reverts to them.
        repo.stored = repo.stored.copyWith(
          lyrics: const [
            LyricLine(text: 'x'),
            LyricLine(text: 'y'),
          ],
        );
        repo.failNextSave = true;
        await vm.stampNextLyric();
        expect(vm.lyrics.map((l) => l.text).toList(), ['x', 'y']);
        vm.playback.position.value = const Duration(milliseconds: 2000);
        await vm.stampNextLyric();
        expect(
          vm.lyrics[0].timeMs,
          2000,
          reason:
              'Space did nothing: cursor ${vm.syncIndex} of ${vm.lyrics.length}',
        );
      },
    );
  });
  group('the sync cursor through failed and queued writes', () {
    Future<void> waitInFlight() async {
      for (var i = 0; i < 50 && repo.inFlight == 0; i++) {
        await Future<void>.microtask(() {});
      }
      expect(
        repo.inFlight,
        1,
        reason: 'probe setup: a write should be in flight',
      );
    }

    test(
      'Sync opened while a failed same-words re-import is in flight',
      () async {
        final vm = await pumpVm();
        await vm.setLyricsText('a\nb\nc\nd\ne\nf');
        vm.setLyricsMode(LyricsMode.sync);
        vm.playback.position.value = const Duration(milliseconds: 1000);
        for (var i = 0; i < 3; i++) {
          await vm.stampNextLyric();
        }
        vm.setLyricsMode(LyricsMode.read); // Done
        repo.failSaves = true;
        final landed = vm.importLyrics('a\nb\nc\nd\ne\nf'); // drop in Read mode
        await waitInFlight();
        vm.setLyricsMode(
          LyricsMode.sync,
        ); // the user opens Sync while it writes
        expect(await landed, isFalse);
        repo.failSaves = false;
        expect(vm.lyrics.take(3).every((l) => l.timeMs == 1000), isTrue);
        vm.playback.position.value = const Duration(milliseconds: 7000);
        await vm.stampNextLyric();
        expect(
          vm.lyrics[0].timeMs,
          1000,
          reason: 'Space overwrote line a (cursor was ${vm.syncIndex})',
        );
        expect(vm.lyrics[3].timeMs, 7000, reason: 'd never got the tap');
      },
    );

    test('the same, with Sync toggled off and on', () async {
      final vm = await pumpVm();
      await vm.setLyricsText('a\nb\nc\nd\ne\nf');
      vm.setLyricsMode(LyricsMode.sync);
      vm.playback.position.value = const Duration(milliseconds: 1000);
      for (var i = 0; i < 3; i++) {
        await vm.stampNextLyric();
      }
      repo.failSaves = true;
      final landed = vm.importLyrics('a\nb\nc\nd\ne\nf');
      await waitInFlight();
      vm.setLyricsMode(LyricsMode.read);
      vm.setLyricsMode(LyricsMode.sync);
      expect(await landed, isFalse);
      repo.failSaves = false;
      vm.playback.position.value = const Duration(milliseconds: 7000);
      await vm.stampNextLyric();
      expect(vm.lyrics[0].timeMs, 1000, reason: 'Space overwrote line a');
      expect(vm.lyrics[3].timeMs, 7000, reason: 'd never got the tap');
    });

    test(
      'a failed Clear never leaves Space on a line the disk holds timed',
      () async {
        final vm = await pumpVm();
        await vm.setLyricsText('a\nb\nc\nd\ne\nf');
        vm.setLyricsMode(LyricsMode.sync);
        vm.playback.position.value = const Duration(milliseconds: 1000);
        for (var i = 0; i < 3; i++) {
          await vm.stampNextLyric();
        }
        repo.failSaves = true;
        await vm.clearLyricTimes();
        repo.failSaves = false;
        expect(vm.lyrics.take(3).every((l) => l.timeMs == 1000), isTrue);
        vm.playback.position.value = const Duration(milliseconds: 7000);
        await vm.stampNextLyric();
        expect(
          vm.lyrics[0].timeMs,
          1000,
          reason: 'Space overwrote line a (cursor ${vm.syncIndex})',
        );
        expect(vm.lyrics[3].timeMs, 7000);
      },
    );

    test('a Clear during a failed same-words re-import, both failing', () async {
      final vm = await pumpVm();
      await vm.setLyricsText('a\nb\nc\nd\ne\nf');
      vm.setLyricsMode(LyricsMode.sync);
      vm.playback.position.value = const Duration(milliseconds: 1000);
      for (var i = 0; i < 3; i++) {
        await vm.stampNextLyric();
      }
      repo.failSaves = true;
      final landed = vm.importLyrics(
        '[00:01.00]a\n[00:02.00]b\n[00:03.00]c\n[00:04.00]d\n[00:05.00]e\n[00:06.00]f',
      );
      await waitInFlight();
      final clear = vm.clearLyricTimes();
      expect(await landed, isFalse);
      await clear;
      repo.failSaves = false;
      vm.playback.position.value = const Duration(milliseconds: 7000);
      await vm.stampNextLyric();
      expect(vm.lyrics[0].timeMs, 1000, reason: 'Space overwrote line a');
    });

    test('two taps inside one round-trip whose writes both fail', () async {
      final vm = await pumpVm();
      await vm.setLyricsText('a\nb\nc\nd');
      vm.setLyricsMode(LyricsMode.sync);
      repo.failSaves = true;
      vm.playback.position.value = const Duration(milliseconds: 1000);
      final t1 = vm.stampNextLyric();
      await waitInFlight(); // tap 1's write is in flight, sent with a stamped
      vm.playback.position.value = const Duration(milliseconds: 2000);
      final t2 = vm.stampNextLyric();
      await Future.wait([t1, t2]);
      repo.failSaves = false;
      expect(vm.lyrics.every((l) => l.timeMs == null), isTrue);
      expect(
        vm.syncIndex,
        0,
        reason: 'cursor on ${vm.syncIndex}: line a lost its tap and is skipped',
      );
    });

    test(
      'three taps in one round-trip whose middle write fails once',
      () async {
        final vm = await pumpVm();
        await vm.setLyricsText('a\nb\nc\nd\ne');
        vm.setLyricsMode(LyricsMode.sync);
        vm.playback.position.value = const Duration(milliseconds: 1000);
        final t0 = vm.stampNextLyric(); // write 0 in flight
        await waitInFlight();
        repo.failAt = {repo.saveNo + 2}; // the next write after it fails once
        vm.playback.position.value = const Duration(milliseconds: 2000);
        final t1 = vm.stampNextLyric();
        vm.playback.position.value = const Duration(milliseconds: 3000);
        final t2 = vm.stampNextLyric();
        await Future.wait([t0, t1, t2]);
        await Future<void>.delayed(Duration.zero);
        expect(repo.stored.lyrics.map((l) => l.timeMs).toList(), [
          1000,
          null,
          null,
          null,
          null,
        ]);
        expect(
          vm.syncIndex,
          1,
          reason: 'b and c lost their taps, cursor on ${vm.syncIndex}',
        );
      },
    );

    test(
      'a tap on a re-import whose write fails once behind another save',
      () async {
        final vm = await pumpVm();
        await vm.setLyricsText('a\nb\nc\nd\ne\nf');
        vm.setLyricsMode(LyricsMode.sync);
        vm.playback.position.value = const Duration(milliseconds: 1000);
        for (var i = 0; i < 3; i++) {
          await vm.stampNextLyric();
        }
        final move = vm
            .commitTrackMove(); // any save in flight (a fader/drag release)
        await waitInFlight();
        repo.failAt = {repo.saveNo + 2}; // the import's write fails once
        final landed = vm.importLyrics('a\nb\nc\nd\ne\nf');
        expect(vm.syncIndex, 0); // a frame
        vm.playback.position.value = const Duration(milliseconds: 2000);
        final tap = vm.stampNextLyric();
        await move;
        expect(await landed, isFalse);
        await tap;
        await Future<void>.delayed(Duration.zero);
        expect(
          repo.stored.lyrics.take(3).every((l) => l.timeMs == 1000),
          isTrue,
        );
        vm.playback.position.value = const Duration(milliseconds: 7000);
        await vm.stampNextLyric();
        expect(
          repo.stored.lyrics[1].timeMs,
          1000,
          reason: 'Space overwrote b\'s good time on disk',
        );
        expect(repo.stored.lyrics[3].timeMs, 7000);
      },
    );

    test(
      'Space after a click on the last line stamps a line still untimed',
      () async {
        final vm = await pumpVm();
        await vm.setLyricsText('a\nb\nc\nd');
        vm.setLyricsMode(LyricsMode.sync);
        vm.playback.position.value = const Duration(milliseconds: 9000);
        await vm.stampLyricAt(3);
        vm.playback.position.value = const Duration(milliseconds: 1000);
        await vm.stampNextLyric();
        expect(
          vm.lyrics.where((l) => l.timeMs == 1000),
          isNotEmpty,
          reason:
              'Space did nothing: cursor ${vm.syncIndex} of ${vm.lyrics.length}',
        );
      },
    );
  });
}

bool _sameLyrics(List<LyricLine> a, List<LyricLine> b) =>
    a.length == b.length &&
    [for (var i = 0; i < a.length; i++) a[i] == b[i]].every((e) => e);
