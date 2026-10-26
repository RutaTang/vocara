import 'dart:async';
import 'dart:io';

import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/asset_store.dart';
import 'package:vocara/data/repositories/song_practice_repository_drift.dart';
import 'package:vocara/data/repositories/technique_repository_drift.dart';
import 'package:vocara/data/services/app_settings_service.dart';
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
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/ui/song_practice_detail/view_model/song_practice_detail_view_model.dart';
import 'package:vocara/utils/id.dart';
import 'package:vocara/utils/result.dart';

/// A do-nothing engine so the view model can be exercised without media_kit
/// or the native graph engine (mirrors the mixer test's fake, plus the
/// position/playing streams the playback controller subscribes to).
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

void main() {
  late AppDatabase db;
  late Directory tmp;
  late SongPracticeRepositoryDrift repo;
  late TechniqueRepositoryDrift techRepo;
  late String practiceId;
  late List<int> takeBytes;
  late List<int> clipABytes;
  late List<int> clipBBytes;

  Future<String> writeAudio(String name, List<int> bytes) async {
    final f = File(p.join(tmp.path, name));
    await f.writeAsBytes(bytes);
    return f.path;
  }

  setUp(() async {
    db = AppDatabase(NativeDatabase.memory());
    tmp = await Directory.systemTemp.createTemp('vocara_export_take');
    final writer = SyncWriter(db, HlcClock(node: 'NODE'.padRight(26, '0')));
    final assets = AssetStore(db: db, root: tmp, writer: writer);
    repo = SongPracticeRepositoryDrift(db: db, writer: writer, assets: assets);
    techRepo = TechniqueRepositoryDrift(db: db, writer: writer, assets: assets);

    // Seed one practice: a backing, a single-clip take and a multi-clip take.
    final created = await repo.createPractice('My Song');
    practiceId = (created as Ok<SongPractice>).value.id;
    final loaded =
        await repo.loadPractice(practiceId) as Ok<Loaded<SongPractice>>;

    takeBytes = List<int>.generate(96, (i) => i);
    clipABytes = List<int>.filled(64, 1);
    clipBBytes = List<int>.filled(64, 2);
    final now = DateTime.now();
    final single = AudioTrack(
      id: newId(),
      kind: AudioTrackKind.vocal,
      displayName: 'Take 1',
      audioFilePath: await writeAudio('take1.wav', takeBytes),
      durationMs: 500,
      createdAt: now,
    );
    final multi = AudioTrack(
      id: newId(),
      kind: AudioTrackKind.vocal,
      displayName: 'Take 2',
      createdAt: now,
      clips: [
        AudioClip(
          id: newId(),
          audioFilePath: await writeAudio('clip_a.wav', clipABytes),
          durationMs: 300,
        ),
        AudioClip(
          id: newId(),
          audioFilePath: await writeAudio('clip_b.wav', clipBBytes),
          durationMs: 300,
          startMs: 400,
        ),
      ],
    );
    final backing = AudioTrack(
      id: newId(),
      kind: AudioTrackKind.accompaniment,
      displayName: 'Backing',
      audioFilePath: await writeAudio('backing.mp3', clipABytes),
      durationMs: 500,
      createdAt: now,
    );
    final saved = await repo.savePracticeWithBase(
      loaded.value.aggregate.copyWith(
        accompaniment: backing,
        vocalTracks: [single, multi],
      ),
      baseHashes: loaded.value.baseHashes,
    );
    expect(saved, isA<Ok<Loaded<SongPractice>>>());
  });

  tearDown(() async {
    await db.close();
    if (await tmp.exists()) await tmp.delete(recursive: true);
  });

  /// Builds the view model with an injected save-location picker and waits for
  /// the initial load. [picks] collects every suggested file name.
  Future<SongPracticeDetailViewModel> pumpVm(
    List<String> picks,
    Future<String?> Function(String suggestedName) pick,
  ) async {
    final storage = LocalStorageService();
    final vm = SongPracticeDetailViewModel(
      repository: repo,
      techniqueRepository: techRepo,
      audioFileService: AudioFileService(storage: storage),
      waveformService: WaveformService(storage: storage),
      spectrogramService: const SpectrogramService(),
      separationService: FakeSourceSeparationService(available: false),
      appSettings: AppSettingsService(storage: storage),
      practiceId: practiceId,
      playbackEngine: _FakeEngine(),
      saveLocationPicker: (name) {
        picks.add(name);
        return pick(name);
      },
    );
    addTearDown(vm.dispose);
    for (var i = 0; i < 400 && vm.practice == null; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }
    expect(vm.practice, isNotNull, reason: 'initial load should complete');
    return vm;
  }

  AudioTrack byName(SongPracticeDetailViewModel vm, String name) =>
      vm.practice!.vocalTracks.firstWhere((t) => t.displayName == name);

  test('exports a single-clip take byte-for-byte to the picked path', () async {
    final dest = p.join(tmp.path, 'exported.wav');
    final picks = <String>[];
    final vm = await pumpVm(picks, (_) async => dest);
    final toasts = <String>[];
    vm.toasts.listen(toasts.add);

    await vm.exportTrackAudio(byName(vm, 'Take 1').id);
    await Future<void>.delayed(Duration.zero);

    expect(picks.single, 'My Song - Take 1.wav');
    expect(await File(dest).readAsBytes(), takeBytes);
    expect(toasts, contains('Exported to $dest'));
  });

  test(
    'an export onto the take\'s own file is refused, its audio kept',
    () async {
      // The save panel can browse into the app's storage: copying a file onto
      // itself truncated it to nothing, and the toast said "Exported".
      final picks = <String>[];
      late final String own;
      final vm = await pumpVm(picks, (_) async => own);
      own = byName(vm, 'Take 1').primaryClip!.audioFilePath;
      final toasts = <String>[];
      vm.toasts.listen(toasts.add);

      await vm.exportTrackAudio(byName(vm, 'Take 1').id);
      await Future<void>.delayed(Duration.zero);

      expect(await File(own).readAsBytes(), takeBytes);
      expect(toasts.single, contains('own audio file'));
    },
  );

  test('a cancelled save panel exports nothing and stays silent', () async {
    final picks = <String>[];
    final vm = await pumpVm(picks, (_) async => null);
    final toasts = <String>[];
    vm.toasts.listen(toasts.add);

    await vm.exportTrackAudio(byName(vm, 'Take 1').id);
    await Future<void>.delayed(Duration.zero);

    expect(picks, hasLength(1)); // the panel opened…
    expect(toasts, isEmpty); // …but a cancel is not an outcome to report
  });

  test('refuses a multi-clip take without opening the save panel', () async {
    final picks = <String>[];
    final vm = await pumpVm(picks, (_) async => p.join(tmp.path, 'x.wav'));
    final toasts = <String>[];
    vm.toasts.listen(toasts.add);

    await vm.exportTrackAudio(byName(vm, 'Take 2').id);
    await Future<void>.delayed(Duration.zero);

    expect(picks, isEmpty);
    expect(toasts.single, contains('multiple clips'));
    expect(File(p.join(tmp.path, 'x.wav')).existsSync(), isFalse);
  });

  test('exports one clip of a multi-clip take with a numbered name', () async {
    final dest = p.join(tmp.path, 'clip_export.wav');
    final picks = <String>[];
    final vm = await pumpVm(picks, (_) async => dest);

    final take = byName(vm, 'Take 2');
    await vm.exportClipAudio(take.id, take.clips[1].id);

    expect(picks.single, 'My Song - Take 2 (clip 2).wav');
    expect(await File(dest).readAsBytes(), clipBBytes);
  });

  test('keeps the source extension for a backing export', () async {
    final dest = p.join(tmp.path, 'backing_export.mp3');
    final picks = <String>[];
    final vm = await pumpVm(picks, (_) async => dest);

    await vm.exportTrackAudio(vm.practice!.accompaniment!.id);

    expect(picks.single, 'My Song - Backing.mp3');
    expect(File(dest).existsSync(), isTrue);
  });

  test('a missing source file toasts instead of opening the panel', () async {
    final picks = <String>[];
    final vm = await pumpVm(picks, (_) async => p.join(tmp.path, 'y.wav'));
    final toasts = <String>[];
    vm.toasts.listen(toasts.add);

    final take = byName(vm, 'Take 1');
    File(take.primaryClip!.audioFilePath).deleteSync();
    await vm.exportTrackAudio(take.id);
    await Future<void>.delayed(Duration.zero);

    expect(picks, isEmpty);
    expect(toasts.single, contains('missing'));
  });

  test('separation stays gated: probe false + plain-language toast', () async {
    final picks = <String>[];
    final vm = await pumpVm(picks, (_) async => null);
    final toasts = <String>[];
    vm.toasts.listen(toasts.add);

    // The Null/unavailable engine resolves the probe to false, which is what
    // hides "Extract backing" in the menu (canSeparate && separationAvailable).
    expect(vm.separationAvailable, isFalse);

    // Residual race (click before the probe landed): user language, no dev speak.
    await vm.separateBacking(vm.practice!.accompaniment!.id);
    await Future<void>.delayed(Duration.zero);
    expect(
      toasts.single,
      'Source separation is not available in this version.',
    );
    expect(vm.isSeparating, isFalse);
  });
}
