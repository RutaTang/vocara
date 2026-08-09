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
import 'package:vocara/domain/models/lyric_line.dart';
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
  int inFlight = 0;
  int maxInFlight = 0;

  @override
  Future<Result<Loaded<SongPractice>>> loadPractice(String id) async =>
      Result.ok(Loaded(stored, const {}));

  @override
  Future<Result<Loaded<SongPractice>>> savePracticeWithBase(
    SongPractice practice, {
    required Map<String, String> baseHashes,
  }) async {
    inFlight++;
    if (inFlight > maxInFlight) maxInFlight = inFlight;
    // Yield, so an overlapping (non-serialized) second save would be caught
    // by the in-flight counter.
    await Future<void>.delayed(Duration.zero);
    inFlight--;
    if (failSaves || failNextSave) {
      failNextSave = false;
      events.add('save-failed');
      return Result.error(Exception('save failed'));
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

  @override
  Future<String> importAudio({
    required String practiceId,
    required String sourcePath,
  }) async {
    await Future<void>.delayed(Duration.zero);
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

  @override
  Future<String> duplicate({
    required String practiceId,
    required String sourcePath,
  }) async => '/edit/dup${_dups++}.wav';
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

SongPractice _seedPractice({
  List<AudioTrack>? vocalTracks,
  AudioTrack? reference,
}) => SongPractice(
  id: 'p1',
  title: 'Song',
  createdAt: DateTime(2026),
  updatedAt: DateTime(2026),
  reference: reference,
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
  }) async {
    events = <String>[];
    repo = _FakeRepo(
      _seedPractice(vocalTracks: vocalTracks, reference: reference),
      events,
    );
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
      playbackEngine: _FakeEngine(),
      editService: edits ?? _FakeEdits(),
      probeDuration: probeDuration,
    );
    addTearDown(vm.dispose);
    for (var i = 0; i < 400 && vm.practice == null; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }
    expect(vm.practice, isNotNull, reason: 'initial load should complete');
    events.clear();
    return vm;
  }

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

    test('merge on a FAILED save keeps the sources and cleans up the '
        'merged output instead', () async {
      final vm = await pumpVm(vocalTracks: _mergeableTakes());
      repo.failSaves = true;
      await vm.mergeTrackDown('top');

      final deletes = events.where((e) => e.startsWith('delete:')).toList();
      expect(deletes, ['delete:/edit/merged.wav']);
      expect(vm.practice!.vocalTracks.map((t) => t.id), ['top', 'bottom']);
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

  group('lyrics save failure', () {
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
}
