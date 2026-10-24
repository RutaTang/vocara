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
import 'package:vocara/data/services/source_separation_service.dart';
import 'package:vocara/data/services/spectrogram_service.dart';
import 'package:vocara/data/services/waveform_service.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/sync_hashes.dart';
import 'package:vocara/data/sync/sync_writer.dart';
import 'package:vocara/domain/models/audio_fx.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/fx_bus.dart';
import 'package:vocara/domain/models/lyric_line.dart';
import 'package:vocara/domain/models/practice_note.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/domain/models/technique_marker.dart';
import 'package:vocara/ui/song_practice_detail/view_model/song_practice_detail_view_model.dart';
import 'package:vocara/utils/id.dart';
import 'package:vocara/utils/result.dart';

/// The base a queued save merges against, end to end on the real drift store.
///
/// When a second edit lands while a save is in flight, the view model keeps
/// its newer model rather than adopting the re-read aggregate. It used to
/// adopt the re-read's FRESH base anyway — but that base includes rows the
/// three-way rules kept from a concurrent writer, rows the newer model has
/// never seen. The queued save then merged a stale model against a base that
/// knew about them: a remote insert was tombstoned (in base, not desired), a
/// remote edit reverted (mine != base) and a remote delete resurrected.

class _FakeTechRepo extends Fake implements TechniqueRepository {}

class _NoopFiles extends Fake implements AudioFileService {
  @override
  Future<void> deleteAudio(String path) async {}
}

class _FakeWaveforms extends Fake implements WaveformService {}

class _FakeEdits extends Fake implements AudioEditService {}

/// Just enough engine surface for the playback/mixer controllers to wire up;
/// counts graph syncs.
class _FakeEngine extends Fake implements PlaybackEngine {
  int syncs = 0;

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
  ) async => syncs++;
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
  late SongPracticeRepositoryDrift repo;

  setUp(() async {
    db = AppDatabase(NativeDatabase.memory());
    tmp = await Directory.systemTemp.createTemp('vocara_save_base');
    final writer = SyncWriter(db, HlcClock(node: 'NODE'.padRight(26, '0')));
    repo = SongPracticeRepositoryDrift(
      db: db,
      writer: writer,
      assets: AssetStore(db: db, root: tmp, writer: writer),
    );
  });
  tearDown(() async {
    await db.close();
    if (await tmp.exists()) await tmp.delete(recursive: true);
  });

  PracticeNote note(String content) => PracticeNote(
    id: newId(),
    positionMs: 10,
    content: content,
    createdAt: DateTime(2026),
    updatedAt: DateTime(2026),
  );

  Future<Loaded<SongPractice>> load(String id) async =>
      (await repo.loadPractice(id) as Ok<Loaded<SongPractice>>).value;

  /// A practice with one take carrying [notes], persisted.
  Future<SongPractice> seed({List<PracticeNote> notes = const []}) async {
    final created =
        (await repo.createPractice('Song') as Ok<SongPractice>).value;
    final take = AudioTrack(
      id: newId(),
      kind: AudioTrackKind.vocal,
      displayName: 'Take 1',
      createdAt: DateTime(2026),
      versionId: created.activeVersionId,
      notes: notes,
    );
    await repo.savePractice(created.copyWith(vocalTracks: [take]));
    return (await load(created.id)).aggregate;
  }

  late _FakeEngine engine;

  Future<SongPracticeDetailViewModel> open(String practiceId) async {
    engine = _FakeEngine();
    final vm = SongPracticeDetailViewModel(
      repository: repo,
      techniqueRepository: _FakeTechRepo(),
      audioFileService: _NoopFiles(),
      waveformService: _FakeWaveforms(),
      spectrogramService: const SpectrogramService(),
      separationService: FakeSourceSeparationService(available: false),
      appSettings: AppSettingsService(storage: LocalStorageService()),
      practiceId: practiceId,
      playbackEngine: engine,
      editService: _FakeEdits(),
    );
    addTearDown(vm.dispose);
    for (var i = 0; i < 400 && vm.practice == null; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }
    expect(vm.practice, isNotNull);
    return vm;
  }

  /// Another writer (the sync applier, another editor) changes the take's
  /// notes while the screen holds the aggregate.
  Future<void> concurrently(
    String practiceId,
    List<PracticeNote> Function(List<PracticeNote> notes) edit,
  ) async {
    final other = await load(practiceId);
    final take = other.aggregate.vocalTracks.single;
    final r = await repo.savePracticeWithBase(
      other.aggregate.copyWith(
        vocalTracks: [take.copyWith(notes: edit(take.notes))],
      ),
      baseHashes: other.baseHashes,
    );
    expect(r, isA<Ok<Loaded<SongPractice>>>());
  }

  /// Two quick local edits: the second lands while the first save is still
  /// in flight, so the model moves on under it.
  Future<void> renameTwice(
    SongPracticeDetailViewModel vm,
    String takeId,
  ) async {
    final first = vm.renameTrack(takeId, 'A');
    await null;
    final second = vm.renameTrack(takeId, 'B');
    await Future.wait([first, second]);
  }

  test('a concurrent insert survives the queued save', () async {
    final p = await seed();
    final vm = await open(p.id);
    final remote = note('from the other device');
    await concurrently(p.id, (notes) => [...notes, remote]);

    await renameTwice(vm, p.vocalTracks.single.id);

    final take = (await load(p.id)).aggregate.vocalTracks.single;
    expect(take.displayName, 'B');
    expect(take.notes.map((n) => n.id), [remote.id]);
  });

  test('a concurrent edit survives the queued save', () async {
    final held = note('as loaded');
    final p = await seed(notes: [held]);
    final vm = await open(p.id);
    await concurrently(
      p.id,
      (notes) => [notes.single.copyWith(content: 'edited elsewhere')],
    );

    await renameTwice(vm, p.vocalTracks.single.id);

    final take = (await load(p.id)).aggregate.vocalTracks.single;
    expect(take.displayName, 'B');
    expect(take.notes.single.content, 'edited elsewhere');
  });

  test('a concurrent delete stays deleted after the queued save', () async {
    final p = await seed(notes: [note('to be deleted elsewhere')]);
    final vm = await open(p.id);
    await concurrently(p.id, (_) => const []);

    await renameTwice(vm, p.vocalTracks.single.id);

    final take = (await load(p.id)).aggregate.vocalTracks.single;
    expect(take.displayName, 'B');
    expect(take.notes, isEmpty);
  });

  test(
    'a lyrics save that merged a concurrent change resyncs the engine',
    () async {
      // The lyrics path skips the engine resync so tap-to-time never glitches
      // playback, and remote reloads are skipped while a save is in flight —
      // so a concurrent change the lyrics save merged (a take muted or deleted
      // elsewhere) showed in the UI while the engine kept playing the old mix.
      final p = await seed();
      final vm = await open(p.id);
      await Future<void>.delayed(const Duration(milliseconds: 20));

      final quiet = engine.syncs;
      expect(await vm.setLyricsText('first line'), isTrue);
      expect(engine.syncs, quiet, reason: 'a plain lyric save stays quiet');

      final other = await load(p.id);
      final take = other.aggregate.vocalTracks.single;
      await repo.savePracticeWithBase(
        other.aggregate.copyWith(vocalTracks: [take.copyWith(muted: true)]),
        baseHashes: other.baseHashes,
      );
      expect(await vm.setLyricsText('second line'), isTrue);

      expect(vm.practice!.vocalTracks.single.muted, isTrue);
      expect(engine.syncs, greaterThan(quiet), reason: 'the mix must follow');
    },
  );

  test(
    'a save reports what it sent in the same hashes a load reports',
    () async {
      // With nothing concurrent, the sent state IS the stored state, so the
      // two maps must agree key for key: a save-side projection that drifted
      // from the row hash would read every untouched row as edited.
      final p = await seed(notes: [note('n')]);
      final loaded = await load(p.id);
      final take = loaded.aggregate.vocalTracks.single;
      final full = loaded.aggregate.copyWith(
        lyrics: const [LyricLine(text: 'la', timeMs: 100)],
        reverb: const ReverbSettings(),
        fxBuses: [FxBus(id: newId(), name: 'Bus', fx: TrackFx.flat())],
        vocalTracks: [
          take.copyWith(
            fx: TrackFx.flat(),
            techniqueMarkers: [
              TechniqueMarker(
                id: newId(),
                positionMs: 5,
                techniqueId: newId(),
                techniqueName: 'Twang',
                createdAt: DateTime(2026),
                updatedAt: DateTime(2026),
              ),
            ],
          ),
        ],
      );
      final saved =
          (await repo.savePracticeWithBase(full, baseHashes: loaded.baseHashes)
                  as Ok<Loaded<SongPractice>>)
              .value;
      expect(saved.sentHashes, saved.baseHashes);
    },
  );
}
