// Where new audio lands on the timeline, and what the loop region covers once
// it has: a clip dropped or picked "at the playhead" while playback runs on,
// and a practice whose content outgrows the region it opened with.

import 'dart:async';

import 'package:desktop_drop/desktop_drop.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:forui/forui.dart' show FIcons;
import 'package:provider/provider.dart';
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
import 'package:vocara/domain/models/spectrogram.dart';
import 'package:vocara/domain/models/voice_metrics.dart';
import 'package:vocara/ui/song_practice_detail/view_model/song_practice_detail_view_model.dart';
import 'package:vocara/ui/song_practice_detail/widgets/song_practice_detail_screen.dart';
import 'package:vocara/utils/result.dart';

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
    stored = practice;
    return Result.ok(Loaded(stored, const {}));
  }
}

class _FakeTechRepo extends Fake implements TechniqueRepository {}

class _FakeFiles extends Fake implements AudioFileService {
  int _imports = 0;

  @override
  Future<String> importAudio({
    required String practiceId,
    required String sourcePath,
  }) async => '/import/${_imports++}.wav';

  @override
  Future<void> deleteAudio(String path) async {}
}

class _FakeEdits extends Fake implements AudioEditService {
  @override
  Future<String> ensureStreamableWav({
    required String practiceId,
    required String path,
  }) async => path;
}

/// Each analyze parks until the test releases it — the seconds a real
/// analysis takes, during which playback runs on.
class _GatedWaveforms extends Fake implements WaveformService {
  _GatedWaveforms({this.durationMs = 1000});

  final int durationMs;
  final List<Completer<void>> gates = [];
  bool gated = true;

  @override
  Future<WaveformResult> analyze(
    String audioFilePath, {
    int targetBuckets = 1000,
  }) async {
    if (gated) {
      final gate = Completer<void>();
      gates.add(gate);
      await gate.future;
    }
    return (peaks: const <double>[], durationMs: durationMs);
  }
}

/// No analysis: the inspector and dock have nothing to show, which is all
/// these tests need of them.
class _NoSpectrograms extends Fake implements SpectrogramService {
  @override
  Future<TakeAnalysisBundle?> computeBothFromFile(
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
  Future<Spectrogram?> computeFromFile(
    String path, {
    int fftSize = 2048,
    int hop = 512,
    double maxHz = 5000,
  }) async => null;
}

class _FakeSeparation extends Fake implements SourceSeparationService {
  @override
  Future<bool> isAvailable() async => false;
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
  void setReverb(ReverbSettings reverb) {}
  @override
  Future<void> dispose() async {}
}

SongPractice _practice({List<AudioTrack> vocalTracks = const []}) =>
    SongPractice(
      id: 'p1',
      title: 'Song',
      createdAt: DateTime(2026),
      updatedAt: DateTime(2026),
      vocalTracks: vocalTracks,
    );

/// A one-clip take on a 60 s timeline, to drop clips onto.
final _take = AudioTrack(
  id: 't1',
  kind: AudioTrackKind.vocal,
  displayName: 'Take 1',
  audioFilePath: '/pool/t1.wav',
  durationMs: 60000,
  createdAt: DateTime(2026),
);

SongPracticeDetailViewModel _vm(SongPractice stored, WaveformService waves) =>
    SongPracticeDetailViewModel(
      repository: _FakeRepo(stored),
      techniqueRepository: _FakeTechRepo(),
      audioFileService: _FakeFiles(),
      waveformService: waves,
      spectrogramService: _NoSpectrograms(),
      separationService: _FakeSeparation(),
      appSettings: AppSettingsService(
        storage: LocalStorageService(),
        enumerateInputs: () => const [],
        enumerateOutputs: () => const [],
      ),
      practiceId: 'p1',
      playbackEngine: _FakeEngine(),
      editService: _FakeEdits(),
      probeDuration: (_) => null,
    );

/// The clips added to take t1 beyond its own, by start.
List<int> _addedStarts(SongPracticeDetailViewModel vm) => [
  for (final c in vm.practice!.vocalTracks.single.clips)
    if (c.audioFilePath != '/pool/t1.wav') c.startMs,
];

void main() {
  group('the loop region', () {
    // Regression guard: a new practice opens on the 20 s placeholder
    // timeline, and the region only ever shrank — so after the first import
    // the loop cycled the first 20 seconds of a three-minute song.
    test(
      'covers a first import longer than the placeholder timeline',
      () async {
        final waves = _GatedWaveforms(durationMs: 180000)..gated = false;
        final vm = _vm(_practice(), waves);
        addTearDown(vm.dispose);
        for (var i = 0; i < 400 && vm.practice == null; i++) {
          await Future<void>.delayed(const Duration(milliseconds: 5));
        }
        expect(vm.playback.loopEndMs, 20000, reason: 'empty: the placeholder');

        await vm.addVocalTake('/src/backing-vocal.wav');

        expect(vm.practice!.contentEndMs, 180000);
        expect(vm.playback.loopEndMs, 180000);
      },
    );
  });

  group('a clip added at the playhead', () {
    test('lands at the position its caller captured, not where playback '
        'has moved on to during the import', () async {
      final waves = _GatedWaveforms();
      final vm = _vm(_practice(vocalTracks: [_take]), waves);
      addTearDown(vm.dispose);
      for (var i = 0; i < 400 && vm.practice == null; i++) {
        await Future<void>.delayed(const Duration(milliseconds: 5));
      }
      vm.playback.position.value = const Duration(milliseconds: 9000);

      final adding = vm.addClipToTake('t1', '/src/a.wav', atMs: 5000);
      for (var i = 0; i < 50 && waves.gates.isEmpty; i++) {
        await Future<void>.delayed(Duration.zero);
      }
      vm.playback.position.value = const Duration(milliseconds: 14000);
      waves.gates.single.complete();
      await adding;

      expect(_addedStarts(vm), [5000]);
    });

    Future<SongPracticeDetailViewModel> pumpScreen(
      WidgetTester tester,
      _GatedWaveforms waves,
    ) async {
      tester.view.physicalSize = const Size(1800, 1100);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.reset);
      final vm = _vm(_practice(vocalTracks: [_take]), waves);
      for (var i = 0; i < 100 && vm.practice == null; i++) {
        await tester.pump(const Duration(milliseconds: 5));
      }
      // The lyrics tab bar overflows under the em-wide test font; it is not
      // what these tests are about.
      vm.toggleLyricsPanel();
      await tester.pumpWidget(
        MaterialApp(
          home: ChangeNotifierProvider.value(
            value: vm,
            child: const SongPracticeDetailScreen(),
          ),
        ),
      );
      await tester.pump(const Duration(milliseconds: 50));
      addTearDown(() async {
        await tester.pumpWidget(const SizedBox());
        vm.dispose();
      });
      return vm;
    }

    /// Lets the import awaits run until the next analyze parks (or the
    /// whole drop finished).
    Future<void> settle(WidgetTester tester) async {
      for (var i = 0; i < 20; i++) {
        await tester.pump(const Duration(milliseconds: 10));
      }
    }

    // Regression guard: each file's import read the playhead when it began —
    // after the file before it had been copied, analyzed and saved — so a
    // drop during playback spread its clips across the song.
    testWidgets('every file of a drop lands at the drop playhead', (
      tester,
    ) async {
      final waves = _GatedWaveforms();
      final vm = await pumpScreen(tester, waves);
      vm.playback.position.value = const Duration(milliseconds: 5000);

      final lane = find.ancestor(
        of: find.byKey(const ValueKey('t1')),
        matching: find.byType(DropTarget),
      );
      tester.widget<DropTarget>(lane.first).onDragDone!(
        DropDoneDetails(
          files: [DropItemFile('/src/a.wav'), DropItemFile('/src/b.wav')],
          localPosition: Offset.zero,
          globalPosition: Offset.zero,
        ),
      );
      await settle(tester);
      expect(waves.gates, hasLength(1), reason: 'the first file analyzing');
      // Playback runs on while the first file imports.
      vm.playback.position.value = const Duration(milliseconds: 11000);
      waves.gates.last.complete();
      await settle(tester);
      expect(waves.gates, hasLength(2), reason: 'the second file analyzing');
      waves.gates.last.complete();
      await settle(tester);

      expect(_addedStarts(vm), [5000, 5000]);
    });

    // Regression guard: "Add audio clip at playhead…" read the playhead only
    // once the open panel closed, seconds of playback later.
    testWidgets('a clip picked from the take menu lands where the playhead was '
        'at the tap', (tester) async {
      final waves = _GatedWaveforms()..gated = false;
      final vm = await pumpScreen(tester, waves);
      vm.playback.position.value = const Duration(milliseconds: 5000);
      const channel = MethodChannel('plugins.flutter.io/file_selector');
      tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(channel, (
        call,
      ) async {
        // The user browses while the song plays on.
        vm.playback.position.value = const Duration(milliseconds: 12000);
        return ['/src/picked.wav'];
      });
      addTearDown(
        () => tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
          channel,
          null,
        ),
      );

      await tester.tap(
        find.descendant(
          of: find.byKey(const ValueKey('t1')),
          matching: find.byIcon(FIcons.ellipsis),
        ),
      );
      await tester.pump();
      await tester.tap(find.text('Add audio clip at playhead…'));
      await settle(tester);

      expect(_addedStarts(vm), [5000]);
    });
  });
}
