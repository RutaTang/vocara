// A save still in flight when the screen closes commits, and the saves
// queued behind it run on with the model the user left. The landed save must
// still hand them its fresh base: against the stale one, the store's skip
// rules read a change undone since (an unmute after a mute) as unchanged, and
// dropped it — the store kept "muted" although the user's last word was
// "unmuted".

import 'dart:async';
import 'dart:io';

import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/asset_store.dart';
import 'package:vocara/data/repositories/song_practice_repository_drift.dart';
import 'package:vocara/data/repositories/technique_repository.dart';
import 'package:vocara/data/services/app_settings_service.dart';
import 'package:vocara/data/services/audio_edit_service.dart';
import 'package:vocara/data/services/audio_file_service.dart';
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/data/services/playback_engine.dart';
import 'package:vocara/data/services/recording_finalizer.dart';
import 'package:vocara/data/services/source_separation_service.dart';
import 'package:vocara/data/services/spectrogram_service.dart';
import 'package:vocara/data/services/waveform_service.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/sync_hashes.dart';
import 'package:vocara/data/sync/sync_writer.dart';
import 'package:vocara/domain/models/audio_fx.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/ui/song_practice_detail/view_model/song_practice_detail_view_model.dart';
import 'package:vocara/utils/id.dart';
import 'package:vocara/utils/result.dart';

class _FakeTechRepo extends Fake implements TechniqueRepository {}

class _Parking extends SongPracticeRepositoryDrift {
  _Parking({required super.db, required super.writer, required super.assets});

  Completer<void>? gate;
  int parked = 0;

  @override
  Future<Result<Loaded<SongPractice>>> savePracticeWithBase(
    SongPractice practice, {
    required Map<String, String> baseHashes,
  }) async {
    final g = gate;
    if (g != null) {
      gate = null;
      parked++;
      await g.future;
      parked--;
    }
    return super.savePracticeWithBase(practice, baseHashes: baseHashes);
  }
}

class _NoopFiles extends Fake implements AudioFileService {
  @override
  Future<void> deleteAudio(String path) async {}
}

class _FakeWaveforms extends Fake implements WaveformService {}

class _FakeEdits extends Fake implements AudioEditService {}

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
  Future<void> sync(List<TrackPlayback> t, List<BusPlayback> b) async {}
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

void main() {
  late AppDatabase db;
  late Directory tmp;
  late _Parking repo;

  setUp(() async {
    db = AppDatabase(NativeDatabase.memory());
    tmp = await Directory.systemTemp.createTemp('vocara_dispose_save');
    final writer = SyncWriter(db, HlcClock(node: 'NODE'.padRight(26, '0')));
    repo = _Parking(
      db: db,
      writer: writer,
      assets: AssetStore(db: db, root: tmp, writer: writer),
    );
  });
  tearDown(() async {
    await db.close();
    if (await tmp.exists()) await tmp.delete(recursive: true);
  });

  Future<SongPractice> seed() async {
    final created =
        (await repo.createPractice('Song') as Ok<SongPractice>).value;
    final take = AudioTrack(
      id: newId(),
      kind: AudioTrackKind.vocal,
      displayName: 'Take 1',
      createdAt: DateTime(2026),
      versionId: created.activeVersionId,
    );
    await repo.savePractice(created.copyWith(vocalTracks: [take]));
    return ((await repo.loadPractice(created.id)) as Ok<Loaded<SongPractice>>)
        .value
        .aggregate;
  }

  Future<SongPracticeDetailViewModel> open(String id) async {
    final vm = SongPracticeDetailViewModel(
      repository: repo,
      techniqueRepository: _FakeTechRepo(),
      audioFileService: _NoopFiles(),
      waveformService: _FakeWaveforms(),
      spectrogramService: const SpectrogramService(),
      separationService: FakeSourceSeparationService(available: false),
      appSettings: AppSettingsService(storage: LocalStorageService()),
      practiceId: id,
      playbackEngine: _FakeEngine(),
      editService: _FakeEdits(),
    );
    for (var i = 0; i < 400 && vm.practice == null; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }
    // Let the load's resync and any first save settle.
    await Future<void>.delayed(const Duration(milliseconds: 30));
    return vm;
  }

  Future<bool> storedMute(String practiceId) async =>
      ((await repo.loadPractice(practiceId)) as Ok<Loaded<SongPractice>>)
          .value
          .aggregate
          .vocalTracks
          .single
          .muted;

  for (final leave in [false, true]) {
    test('mute then unmute during one save, leave=$leave', () async {
      final p = await seed();
      final take = p.vocalTracks.single.id;
      final vm = await open(p.id);

      repo.gate = Completer<void>();
      final release = repo.gate!;
      final first = vm.mixer.toggleMute(take); // save A sends muted
      for (var i = 0; i < 200 && repo.parked == 0; i++) {
        await Future<void>.delayed(const Duration(milliseconds: 1));
      }
      expect(repo.parked, 1);
      final second = vm.mixer.toggleMute(take); // queued, model unmuted
      expect(vm.practice!.vocalTracks.single.muted, isFalse);
      if (leave) vm.dispose(); // Back: the screen goes while A is in flight
      release.complete();
      await Future.wait([first, second]);
      await RecordingFinalizer.instance.settled(p.id);

      // The user's last word was "unmuted".
      expect(await storedMute(p.id), isFalse);
      if (!leave) vm.dispose();
    });
  }
}
