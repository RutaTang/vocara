// The inline example players own native media_kit players. These tests swap
// in a fake (a widget test cannot initialize media_kit) to pin that every
// player built is released, that a clip whose file is not on this device
// renders as unavailable instead of throwing, and that a playing example
// survives the page around it scrolling or changing.

import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:media_kit/media_kit.dart';
import 'package:provider/provider.dart';
import 'package:vocara/data/repositories/technique_repository.dart';
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/data/services/media_analysis_service.dart';
import 'package:vocara/data/services/mic_recording_controller.dart';
import 'package:vocara/data/services/technique_media_service.dart';
import 'package:vocara/data/sync/sync_hashes.dart';
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/domain/models/technique_analysis.dart';
import 'package:vocara/domain/models/technique_media.dart';
import 'package:vocara/ui/technique_library/view_model/technique_detail_view_model.dart';
import 'package:vocara/ui/technique_library/widgets/media/technique_audio_player.dart';
import 'package:vocara/ui/technique_library/widgets/media/technique_video_player.dart';
import 'package:vocara/ui/technique_library/widgets/technique_detail_screen.dart';
import 'package:vocara/utils/result.dart';

class _FakeStream extends Fake implements PlayerStream {
  final playingController = StreamController<bool>.broadcast();

  @override
  Stream<bool> get playing => playingController.stream;

  @override
  Stream<Duration> get position => const Stream.empty();

  @override
  Stream<Duration> get duration => const Stream.empty();
}

class _FakePlayer extends Fake implements Player {
  final _FakeStream _stream = _FakeStream();
  bool disposed = false;
  int pauses = 0;

  @override
  PlayerStream get stream => _stream;

  @override
  Future<void> open(Playable playable, {bool play = true}) async {}

  @override
  Future<void> pause() async => pauses++;

  @override
  Future<void> playOrPause() async {}

  @override
  Future<void> seek(Duration duration) async {}

  @override
  Future<void> dispose() async => disposed = true;
}

/// Installs the fake player factory for one test; returns every player built.
List<_FakePlayer> _fakePlayers() {
  final built = <_FakePlayer>[];
  final previous = createExamplePlayer;
  createExamplePlayer = () {
    final player = _FakePlayer();
    built.add(player);
    return player;
  };
  addTearDown(() => createExamplePlayer = previous);
  return built;
}

class _FakeRepo implements TechniqueRepository {
  _FakeRepo(this.technique);
  Technique technique;

  @override
  Future<Result<Technique>> getTechnique(String id) async =>
      Result.ok(technique);

  @override
  Future<Result<Loaded<Technique>>> loadTechnique(String id) async =>
      Result.ok(Loaded(technique, const {}));

  @override
  Future<Result<List<Technique>>> listTechniques() async =>
      Result.ok([technique]);

  @override
  Future<Result<Technique>> saveTechnique(Technique t) async =>
      Result.ok(technique = t);

  @override
  Future<Result<Loaded<Technique>>> saveTechniqueWithBase(
    Technique t, {
    required Map<String, String> baseHashes,
  }) async => Result.ok(Loaded(technique = t, const {}));

  @override
  Future<Result<void>> deleteTechnique(String id) async =>
      const Result.ok(null);
}

/// Analysis that never produces anything (the card shows its failure hint),
/// so these tests stay off the dock views.
class _NoAnalysis extends Fake implements MediaAnalysisService {
  @override
  Future<TechniqueAnalysis?> analyze(
    String path, {
    bool Function()? wanted,
  }) async => null;
}

/// Recorder fake: [start] arms a take in its own temp dir (the view model
/// deletes a take's whole parent dir on cleanup).
class _FakeRecorder extends Fake implements MicRecordingController {
  bool recording = false;
  final ValueNotifier<double> _level = ValueNotifier(0);

  @override
  ValueNotifier<double> get level => _level;

  @override
  bool get isRecording => recording;

  @override
  bool get isStarting => false;

  @override
  bool get feedingMonitor => false;

  @override
  Future<MicRecordingStart> start(RecordingPathStrategy destination) async {
    final dir = await Directory.systemTemp.createTemp('vocara_fake_take');
    recording = true;
    return MicRecordingStart.started('${dir.path}/take.wav');
  }

  @override
  void invalidatePendingStart() {}

  @override
  void dispose() => _level.dispose();
}

const _techniqueId = 'T1000000000000000000000000';

/// A technique with two audio examples (M0, M1) and a description long
/// enough to scroll the examples far out of view.
Technique _technique() {
  final now = DateTime(2026, 6, 20);
  return Technique(
    id: _techniqueId,
    name: 'Twang',
    description: List.filled(400, 'A long paragraph of cues.').join('\n'),
    media: [
      for (final i in [0, 1])
        TechniqueMedia(
          id: 'M$i',
          path: '/tmp/example-$i.wav',
          kind: TechniqueMediaKind.audio,
          label: 'Example $i',
          addedAt: now,
        ),
    ],
    createdAt: now,
    updatedAt: now,
  );
}

Future<TechniqueDetailViewModel> _pumpScreen(
  WidgetTester tester, {
  MicRecordingController? recorder,
}) async {
  tester.view.physicalSize = const Size(1200, 900);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.resetPhysicalSize);
  final vm = TechniqueDetailViewModel(
    repository: _FakeRepo(_technique()),
    mediaService: TechniqueMediaService(storage: LocalStorageService()),
    techniqueId: _techniqueId,
    analysisService: _NoAnalysis(),
    recorder: recorder,
  );
  await tester.pumpWidget(
    MaterialApp(
      home: ChangeNotifierProvider.value(
        value: vm,
        child: const TechniqueDetailScreen(),
      ),
    ),
  );
  await tester.pump(); // let the load command resolve
  await tester.pump();
  return vm;
}

/// The inline audio player of example [id] (its card carries the same key).
Finder _playerOf(String id) => find.byWidgetPredicate(
  (w) => w is TechniqueAudioPlayer && w.key == ValueKey(id),
);

/// The placeholder `asset://` path the repository hydrates an example to when
/// its pooled bytes are missing on this device.
const _missing = 'asset://01J00000000000000000000000';

Widget _host(Widget child) => MaterialApp(
  home: Scaffold(body: Center(child: child)),
);

void main() {
  testWidgets('an audio example whose file is missing shows as unavailable '
      'and leaks no player', (tester) async {
    // Regression: media_kit reads `asset://` as a bundled Flutter asset and
    // threw from Media() AFTER the Player was built; the catch dropped the
    // reference, so every mount leaked a native player nothing disposed.
    final built = _fakePlayers();
    await tester.pumpWidget(_host(const TechniqueAudioPlayer(path: _missing)));

    expect(find.textContaining("isn't on this device"), findsOneWidget);
    await tester.pumpWidget(_host(const SizedBox()));
    expect(
      built.every((p) => p.disposed),
      isTrue,
      reason: 'a player built for the clip must be released with it',
    );
    expect(tester.takeException(), isNull);
  });

  testWidgets('Play on a video example whose file is missing shows it as '
      'unavailable instead of throwing', (tester) async {
    final built = _fakePlayers();
    await tester.pumpWidget(_host(const TechniqueVideoPlayer(path: _missing)));

    await tester.tap(find.byType(TechniqueVideoPlayer));
    await tester.pump();

    expect(tester.takeException(), isNull);
    expect(find.textContaining("isn't on this device"), findsOneWidget);
    await tester.pumpWidget(_host(const SizedBox()));
    expect(built.every((p) => p.disposed), isTrue);
  });

  testWidgets('a video example that fails to open releases its player and '
      'does not throw out of the tap', (tester) async {
    // The fake cannot back a VideoController, standing in for any failure
    // after the player was built.
    final built = _fakePlayers();
    await tester.pumpWidget(
      _host(const TechniqueVideoPlayer(path: '/tmp/example.mp4')),
    );

    await tester.tap(find.byType(TechniqueVideoPlayer));
    await tester.pump();

    expect(tester.takeException(), isNull);
    expect(built, hasLength(1));
    expect(built.single.disposed, isTrue);
    expect(find.textContaining("isn't on this device"), findsOneWidget);
  });

  testWidgets('scrolling the examples out of view keeps their players '
      '(playback does not stop or reset)', (tester) async {
    // Regression: the page is a lazy ListView, so once the Examples block
    // scrolled past the cache extent its players were disposed — a video
    // followed while reading the cues stopped, and came back at 0:00.
    final built = _fakePlayers();
    await _pumpScreen(tester);
    expect(built, hasLength(2));
    final before = tester.state(_playerOf('M1'));

    await tester.drag(find.byType(Scrollable).first, const Offset(0, -6000));
    await tester.pump();
    expect(_playerOf('M1'), findsNothing, reason: 'far away');
    expect(built.where((p) => p.disposed), isEmpty);

    await tester.drag(find.byType(Scrollable).first, const Offset(0, 6000));
    await tester.pump();
    expect(
      identical(tester.state(_playerOf('M1')), before),
      isTrue,
      reason: 'the same player, not a fresh one at 0:00',
    );
    expect(built, hasLength(2));
  });

  testWidgets('removing an earlier example keeps a later card\'s player', (
    tester,
  ) async {
    // Regression: the cards were unkeyed, so removing one handed every later
    // card's slot to the next clip and rebuilt its player — the example
    // playing below the removed one stopped.
    final built = _fakePlayers();
    final vm = await _pumpScreen(tester);
    await tester.tap(find.text('Edit'));
    await tester.pump();
    final player = tester.state(_playerOf('M1'));
    final live = built.where((p) => !p.disposed).toList();

    await vm.removeMedia('M0');
    await tester.pump();

    expect(identical(tester.state(_playerOf('M1')), player), isTrue);
    expect(live.where((p) => p.disposed), hasLength(1), reason: 'only M0');
  });

  testWidgets('starting a practice take pauses every example player', (
    tester,
  ) async {
    // Regression: the examples play through media_kit, not the engine, so the
    // engine's echo canceller has no reference to subtract — a clip left
    // playing on speakers was recorded into the take it was compared with.
    final built = _fakePlayers();
    final vm = await _pumpScreen(tester, recorder: _FakeRecorder());
    expect(built, hasLength(2));

    expect(await tester.runAsync(vm.startPractice), isNull);
    await tester.pump();

    expect(vm.isRecordingPractice, isTrue);
    expect(built.every((p) => p.pauses > 0), isTrue);
  });
}
