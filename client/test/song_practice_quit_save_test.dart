// A quit landing while a practice edit is being persisted must wait for that
// persist. Only a recording's stop-to-save tail used to hold the quit, so an
// edit made in the UI (a rename here; equally an import, a delete, a lyric
// stamp) followed by Cmd+Q exited before its commit and was lost.

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

/// The real drift repository, with every base-hash save parked on [gate]
/// while it is set — a persist still in flight.
class _GatedRepo extends SongPracticeRepositoryDrift {
  _GatedRepo({required super.db, required super.writer, required super.assets});

  Completer<void>? gate;

  @override
  Future<Result<Loaded<SongPractice>>> savePracticeWithBase(
    SongPractice practice, {
    required Map<String, String> baseHashes,
  }) async {
    await gate?.future;
    return super.savePracticeWithBase(practice, baseHashes: baseHashes);
  }
}

class _FakeTechRepo extends Fake implements TechniqueRepository {}

class _NoopFiles extends Fake implements AudioFileService {}

class _FakeWaveforms extends Fake implements WaveformService {}

class _FakeEdits extends Fake implements AudioEditService {}

/// Just enough engine surface for the playback/mixer controllers to wire up.
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
  late _GatedRepo repo;

  setUp(() async {
    db = AppDatabase(NativeDatabase.memory());
    tmp = await Directory.systemTemp.createTemp('vocara_quit_save');
    final writer = SyncWriter(db, HlcClock(node: 'NODE'.padRight(26, '0')));
    repo = _GatedRepo(
      db: db,
      writer: writer,
      assets: AssetStore(db: db, root: tmp, writer: writer),
    );
  });
  tearDown(() async {
    await db.close();
    if (await tmp.exists()) await tmp.delete(recursive: true);
  });

  test('a quit during a queued practice save waits for it to land', () async {
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

    final finalizer = RecordingFinalizer.forTesting();
    final vm = SongPracticeDetailViewModel(
      repository: repo,
      techniqueRepository: _FakeTechRepo(),
      audioFileService: _NoopFiles(),
      waveformService: _FakeWaveforms(),
      spectrogramService: const SpectrogramService(),
      separationService: FakeSourceSeparationService(available: false),
      appSettings: AppSettingsService(storage: LocalStorageService()),
      practiceId: created.id,
      playbackEngine: _FakeEngine(),
      editService: _FakeEdits(),
      finalizer: finalizer,
    );
    addTearDown(vm.dispose);
    for (var i = 0; i < 400 && vm.practice == null; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }
    expect(vm.practice, isNotNull);
    expect(finalizer.hasActive, isFalse, reason: 'nothing in flight yet');

    repo.gate = Completer<void>();
    final renaming = vm.renameTrack(take.id, 'Keeper');
    expect(finalizer.hasActive, isTrue, reason: 'held from the edit on');

    var quitDone = false;
    final quit = finalizer.finalizeAll().whenComplete(() => quitDone = true);
    await Future<void>.delayed(const Duration(milliseconds: 20));
    expect(quitDone, isFalse, reason: 'the quit waits for the airborne save');

    repo.gate!.complete();
    await quit;
    await renaming;
    final stored =
        (await repo.getPractice(created.id) as Ok<SongPractice>).value;
    expect(stored.vocalTracks.single.displayName, 'Keeper');
    expect(finalizer.hasActive, isFalse);
  });
}
