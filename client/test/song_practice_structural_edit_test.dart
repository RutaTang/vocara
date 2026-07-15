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

class _FakeWaveforms extends Fake implements WaveformService {
  @override
  Future<WaveformResult> analyze(
    String audioFilePath, {
    int targetBuckets = 1000,
  }) async => (peaks: const <double>[], durationMs: 1000);
}

class _FakeEdits extends Fake implements AudioEditService {
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

SongPractice _seedPractice({List<AudioTrack>? vocalTracks}) => SongPractice(
  id: 'p1',
  title: 'Song',
  createdAt: DateTime(2026),
  updatedAt: DateTime(2026),
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
  }) async {
    events = <String>[];
    repo = _FakeRepo(_seedPractice(vocalTracks: vocalTracks), events);
    files = _FakeFiles(events);
    final storage = LocalStorageService();
    final vm = SongPracticeDetailViewModel(
      repository: repo,
      techniqueRepository: _FakeTechRepo(),
      audioFileService: files,
      waveformService: _FakeWaveforms(),
      spectrogramService: const SpectrogramService(),
      separationService: FakeSourceSeparationService(available: false),
      appSettings: AppSettingsService(storage: storage),
      practiceId: 'p1',
      playbackEngine: _FakeEngine(),
      editService: _FakeEdits(),
    );
    addTearDown(vm.dispose);
    for (var i = 0; i < 400 && vm.practice == null; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }
    expect(vm.practice, isNotNull, reason: 'initial load should complete');
    events.clear();
    return vm;
  }

  group('structural-edit mutex', () {
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
  });
}
