// The selection the inspector and the dock follow, kept on a lane that exists
// and is on screen whenever the model changes under it: a deleted take, a
// reverted or remotely deleted bus, the backing lane switching sides, a save
// that merged in someone else's take.

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

class _Repo extends Fake implements SongPracticeRepository {
  _Repo(this.stored);
  SongPractice stored;
  bool failNextSave = false;

  /// A row a concurrent writer added, kept by the next save's three-way merge.
  AudioTrack? mergeOnNextSave;

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
    final extra = mergeOnNextSave;
    mergeOnNextSave = null;
    stored = extra == null
        ? practice
        : practice.copyWith(vocalTracks: [...practice.vocalTracks, extra]);
    return Result.ok(
      Loaded(stored, const {
        'x': 'merged',
      }, sentHashes: extra == null ? null : const {'x': 'mine'}),
    );
  }
}

class _TechRepo extends Fake implements TechniqueRepository {}

class _Files extends Fake implements AudioFileService {
  @override
  Future<void> deleteAudio(String path) async {}
}

class _Waveforms extends Fake implements WaveformService {}

class _Edits extends Fake implements AudioEditService {}

class _CountingSpectro extends SpectrogramService {
  _CountingSpectro();
  final List<String> asked = [];

  @override
  Future<TakeAnalysisBundle?> computeBothFromFile(
    String path, {
    int fftSize = 2048,
    int hop = 512,
    double maxHz = 5000,
  }) async {
    asked.add(path);
    return null;
  }
}

class _Engine extends Fake implements PlaybackEngine {
  @override
  Stream<Duration> get position => const Stream.empty();
  @override
  Stream<bool> get playing => const Stream.empty();
  @override
  Duration get currentPosition => Duration.zero;
  @override
  bool get isPlaying => false;
  @override
  bool get supportsEffects => true;
  @override
  void setLoop(bool enabled, int startMs, int endMs) {}
  @override
  Future<void> sync(List<TrackPlayback> t, List<BusPlayback> b) async {}
  @override
  Future<void> play() async {}
  @override
  Future<void> pause() async {}
  @override
  Future<void> seek(Duration position) async {}
  @override
  Future<void> setVolume(String id, double v, {bool muted = false}) async {}
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
  void setTrackSendLevel(String trackId, int slot, double levelDb) {}
  @override
  void setBusSendLevel(String busId, int slot, double levelDb) {}
  @override
  void setReverb(ReverbSettings reverb) {}
  @override
  Future<void> dispose() async {}
}

AudioTrack _take(String id, String path, {String? versionId}) => AudioTrack(
  id: id,
  kind: AudioTrackKind.vocal,
  displayName: id,
  audioFilePath: path,
  durationMs: 1000,
  versionId: versionId,
  createdAt: DateTime(2026),
);

void main() {
  late _Repo repo;
  late _CountingSpectro spectro;

  Future<SongPracticeDetailViewModel> open(SongPractice seed) async {
    repo = _Repo(seed);
    spectro = _CountingSpectro();
    final vm = SongPracticeDetailViewModel(
      repository: repo,
      techniqueRepository: _TechRepo(),
      audioFileService: _Files(),
      waveformService: _Waveforms(),
      spectrogramService: spectro,
      separationService: FakeSourceSeparationService(available: false),
      appSettings: AppSettingsService(storage: LocalStorageService()),
      practiceId: 'p1',
      playbackEngine: _Engine(),
      editService: _Edits(),
    );
    addTearDown(vm.dispose);
    for (var i = 0; i < 400 && vm.practice == null; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }
    await Future<void>.delayed(const Duration(milliseconds: 20));
    return vm;
  }

  SongPractice practice({List<AudioTrack>? takes, List<TakeVersion>? vs}) =>
      SongPractice(
        id: 'p1',
        title: 'Song',
        createdAt: DateTime(2026),
        updatedAt: DateTime(2026),
        versions: vs ?? const [],
        activeVersionId: vs?.first.id,
        vocalTracks:
            takes ?? [_take('t2', '/pool/t2.wav'), _take('t1', '/pool/t1.wav')],
      );

  // The dock computes a take's analysis only when asked (selecting a take
  // asks). A selection moved off a deleted take asked nothing: the dock, open
  // on an analysis tab, stayed blank on the take it moved to.
  test(
    'deleting the selected take analyzes the take the selection moves to',
    () async {
      final vm = await open(practice());
      expect(vm.selectedTrackId, 't2'); // first lane, analyzed (inspector open)
      vm.toggleInspectorPanel(); // inspector collapsed
      vm.analysis.showDockTab(DockTab.pitch); // dock open on an analysis tab
      await Future<void>.delayed(const Duration(milliseconds: 20));
      spectro.asked.clear();

      await vm.deleteTrack('t2');
      await Future<void>.delayed(const Duration(milliseconds: 50));

      expect(vm.selectedTrackId, 't1');
      expect(spectro.asked, contains('/pool/t1.wav'));
    },
  );

  test(
    'deleting the active interpretation analyzes the take it falls back to',
    () async {
      final a = TakeVersion(id: 'A', name: 'A', createdAt: DateTime(2026));
      final b = TakeVersion(id: 'B', name: 'B', createdAt: DateTime(2026));
      final vm = await open(
        practice(
          vs: [a, b],
          takes: [
            _take('ta', '/pool/ta.wav', versionId: 'A'),
            _take('tb', '/pool/tb.wav', versionId: 'B'),
          ],
        ),
      );
      expect(vm.selectedTrackId, 'ta');
      vm.toggleInspectorPanel();
      vm.analysis.showDockTab(DockTab.pitch);
      await Future<void>.delayed(const Duration(milliseconds: 20));
      spectro.asked.clear();

      await vm.versions.delete('A');
      await Future<void>.delayed(const Duration(milliseconds: 50));
      expect(vm.selectedTrackId, 'tb');
      expect(spectro.asked, contains('/pool/tb.wav'));
    },
  );

  // A selected bus that is gone selected nothing at all, for good: an empty
  // inspector and no lane highlighted, through every later save.
  test(
    'a selected bus that no longer exists gives the selection back to a lane',
    () async {
      final vm = await open(practice());
      vm.selectTrack('t1');
      repo.failNextSave = true;
      await vm.mixer.createBus('X');
      await Future<void>.delayed(const Duration(milliseconds: 20));
      expect(vm.mixer.fxBuses, isEmpty); // reverted
      await vm.renameTrack('t1', 'renamed'); // any later save
      expect(vm.selectedBusId == null || vm.selectedBus != null, isTrue);
      expect(vm.selectedTrackId, isNotNull);
    },
  );

  // The backing lane shows one backing at a time; the hidden one was a
  // candidate too, and the inspector then edited a lane neither shown nor heard.
  test(
    'a practice holding only the original never selects its hidden lane',
    () async {
      final ref = AudioTrack(
        id: 'ref',
        kind: AudioTrackKind.reference,
        displayName: 'Original',
        audioFilePath: '/pool/ref.wav',
        durationMs: 1000,
        createdAt: DateTime(2026),
      );
      repo = _Repo(practice());
      final seed = practice().copyWith(reference: ref);
      final vm = await open(seed);
      expect(vm.selectedTrackId, isNot('ref'));
    },
  );

  // A save's re-read can hold a concurrent writer's take past the end of the
  // timeline (merged in while a remote reload was skipped): it sat cut off at
  // the right edge until some later save grew the timeline.
  test(
    'a save that merges in a long take grows the timeline to hold it',
    () async {
      final vm = await open(practice());
      repo.mergeOnNextSave = AudioTrack(
        id: 'remote',
        kind: AudioTrackKind.vocal,
        displayName: 'Remote take',
        audioFilePath: '/pool/remote.wav',
        durationMs: 120000,
        createdAt: DateTime(2026),
      );
      await vm.renameTrack('t1', 'renamed');
      await Future<void>.delayed(const Duration(milliseconds: 20));
      final p = vm.practice!;
      expect(p.timelineDurationMs, greaterThanOrEqualTo(p.contentEndMs));
    },
  );

  test(
    'a selection on the backing lane follows the backing it shows',
    () async {
      AudioTrack backing(String id, AudioTrackKind kind) => AudioTrack(
        id: id,
        kind: kind,
        displayName: id,
        audioFilePath: '/pool/$id.wav',
        durationMs: 1000,
        createdAt: DateTime(2026),
      );
      final vm = await open(
        practice().copyWith(
          accompaniment: backing('acc', AudioTrackKind.accompaniment),
          reference: backing('ref', AudioTrackKind.reference),
        ),
      );
      vm.selectTrack('acc');
      vm.setPlayReference(true);
      expect(vm.selectedTrackId, 'ref');
      vm.setPlayReference(false);
      expect(vm.selectedTrackId, 'acc');
      // A take's selection stays put.
      vm.selectTrack('t1');
      vm.setPlayReference(true);
      expect(vm.selectedTrackId, 't1');
    },
  );
}
