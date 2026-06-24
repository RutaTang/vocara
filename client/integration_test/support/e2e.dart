// Shared harness for the UI-driven end-to-end suites: it pumps the REAL app
// tree (the same `MultiProvider(buildProviders(...))` + `VocaraApp` that
// `main.dart` builds) on the integration embedder, mocks the few platform
// channels the tree brushes against, and provides the primitives the e2e tests
// drive the UI with — simulated OS file drops (`desktop_drop`), a real WAV
// writer, bounded pumping, and repo-level seeding.
//
// The native Rust audio engine is always constructed (createPlaybackEngine), and
// it IS available in the integration build (the audio/analysis suites decode +
// play through it), so recording / playback / FX / mixer all run for real here.
//
// Detail/technique suites deep-link straight into a seeded entity via
// `--dart-define=START=/song/<id>` (or `/techniques/<id>`) using the fixed seed
// ids below; the home/settings suite runs without START and lands on Home.

import 'dart:io';
import 'dart:math' as math;
import 'dart:typed_data';

import 'package:flutter/gestures.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:provider/provider.dart';
import 'package:vocara/app.dart';
import 'package:vocara/config/dependencies.dart';
import 'package:vocara/data/db/persistence.dart';
import 'package:vocara/data/repositories/song_practice_repository.dart';
import 'package:vocara/data/repositories/technique_repository.dart';
import 'package:vocara/data/services/source_separation_service.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/domain/models/take_version.dart';
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/utils/result.dart';

/// Fixed ids so a suite can deep-link via `--dart-define=START=/song/<id>` and
/// seed the same id in setUp. (Any string is a valid PK; these are ULID-shaped.)
const kSeedPracticeId = 'E2EPRACTICESEED0000000000001';
const kSeedVersionId = 'E2EVERSIONSEED00000000000001';
const kSeedTechniqueId = 'E2ETECHNIQUESEED000000000001';

/// Deep-link START values for the suites that boot straight into an entity.
String startSong([String id = kSeedPracticeId]) => '/song/$id';
String startTechnique([String id = kSeedTechniqueId]) => '/techniques/$id';

const _mockedChannels = <String>[
  'plugins.flutter.io/path_provider',
  'plugins.flutter.io/path_provider_macos',
  'window_manager',
  'vocara/window',
  'vocara/mic',
];

/// Mocks the platform channels the real tree touches before/around the first
/// frame so a pump never throws if native plugin timing differs. path_provider
/// returns [tmpDir]; mic permission is granted (so recording isn't blocked);
/// window_manager queries return safe defaults. `desktop_drop` is NOT mocked —
/// [simulateFileDrop] pushes events to the package's own handler.
void installE2eChannelMocks(Directory tmpDir) {
  final messenger =
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
  void mock(String name, Future<Object?>? Function(MethodCall) handler) =>
      messenger.setMockMethodCallHandler(MethodChannel(name), handler);

  for (final ch in const [
    'plugins.flutter.io/path_provider',
    'plugins.flutter.io/path_provider_macos',
  ]) {
    mock(ch, (call) async => call.method.startsWith('get') ? tmpDir.path : null);
  }
  mock('window_manager', (call) async {
    switch (call.method) {
      case 'isMaximized':
      case 'isMinimized':
      case 'isFullScreen':
      case 'isPreventClose':
      case 'isVisible':
      case 'isFocused':
        return false;
      default:
        return null;
    }
  });
  mock('vocara/window', (_) async => null);
  // Mic permission granted so the record path isn't gated in a test.
  mock('vocara/mic', (_) async => true);
}

void removeE2eChannelMocks() {
  final messenger =
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
  for (final ch in _mockedChannels) {
    messenger.setMockMethodCallHandler(MethodChannel(ch), null);
  }
}

/// Pumps the REAL app tree — identical wiring to `main.dart`, with an optional
/// fake separation engine swapped in (the real one shells out to Python).
Future<void> pumpApp(
  WidgetTester tester,
  Persistence persistence, {
  SourceSeparationService? separation,
}) async {
  await tester.pumpWidget(
    MultiProvider(
      providers: buildProviders(persistence, separationOverride: separation),
      child: const VocaraApp(),
    ),
  );
}

/// Pumps repeatedly (capped) until [condition] holds or the cap is hit; returns
/// whether it was satisfied — callers `expect(..., isTrue)` so a timeout reads as
/// a clear failure rather than a hang. Never `pumpAndSettle` the detail screen:
/// it carries indefinite painters/animations that never settle.
Future<bool> pumpUntil(
  WidgetTester tester,
  bool Function() condition, {
  int maxIterations = 80,
  Duration step = const Duration(milliseconds: 100),
}) async {
  for (var i = 0; i < maxIterations; i++) {
    if (condition()) return true;
    await tester.pump(step);
  }
  return condition();
}

/// True once at least one widget matches [finder].
bool present(Finder finder) => finder.evaluate().isNotEmpty;

/// Moves a synthetic mouse pointer onto [finder]'s center so a hover-only
/// affordance (e.g. a card's delete icon, which is only built while the card is
/// hovered) materializes before a test taps it. Without this, the icon's finder
/// returns `findsNothing` by design. The pointer is removed on test teardown.
Future<void> hoverOver(WidgetTester tester, Finder finder) async {
  final gesture = await tester.createGesture(kind: PointerDeviceKind.mouse);
  await gesture.addPointer(location: Offset.zero);
  addTearDown(gesture.removePointer);
  await gesture.moveTo(tester.getCenter(finder));
  await tester.pump();
}

/// Simulates an OS file drop of [paths] onto the `DropTarget` under [at] (a
/// LOGICAL global point, e.g. `tester.getCenter(finder)`). macOS path: send
/// `entered` (so the target under the point goes to `enter` status) then
/// `performOperation_macos` (fires `onDragDone` only on that in-bounds target).
/// macOS doesn't scale the hover point, so logical coords map 1:1.
Future<void> simulateFileDrop(
  WidgetTester tester,
  List<String> paths, {
  required Offset at,
}) async {
  const codec = StandardMethodCodec();
  final messenger =
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
  Future<void> send(String method, Object? args) {
    return messenger.handlePlatformMessage(
      'desktop_drop',
      codec.encodeMethodCall(MethodCall(method, args)),
      (_) {},
    );
  }

  await send('entered', <double>[at.dx, at.dy]);
  await tester.pump();
  await send('updated', <double>[at.dx, at.dy]);
  await tester.pump();
  await send('performOperation_macos', [
    for (final p in paths) <String, Object?>{'path': p, 'isDirectory': false},
  ]);
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 50));
}

/// Writes a mono 16-bit PCM WAV sine tone and returns its path (real audio the
/// engine + analysis can decode).
Future<String> writeSineWav(
  String path, {
  int ms = 600,
  double freq = 220,
}) async {
  const sr = 44100;
  final samples = (sr * ms / 1000).round();
  const bytesPerSample = 2;
  final dataSize = samples * bytesPerSample;
  final b = BytesBuilder();
  void str(String s) => b.add(s.codeUnits);
  void u32(int v) =>
      b.add((ByteData(4)..setUint32(0, v, Endian.little)).buffer.asUint8List());
  void u16(int v) =>
      b.add((ByteData(2)..setUint16(0, v, Endian.little)).buffer.asUint8List());
  str('RIFF');
  u32(36 + dataSize);
  str('WAVE');
  str('fmt ');
  u32(16);
  u16(1); // PCM
  u16(1); // mono
  u32(sr);
  u32(sr * bytesPerSample);
  u16(bytesPerSample);
  u16(16);
  str('data');
  u32(dataSize);
  final pcm = ByteData(dataSize);
  for (var i = 0; i < samples; i++) {
    final v = (0.3 * 32767 * math.sin(2 * math.pi * freq * i / sr)).round();
    pcm.setInt16(i * bytesPerSample, v, Endian.little);
  }
  b.add(pcm.buffer.asUint8List());
  await File(path).writeAsBytes(b.toBytes());
  return path;
}

/// Inserts an empty practice with the fixed seed id (one interpretation, no
/// takes) so a START deep-link boots straight into its detail screen.
Future<void> seedEmptyPractice(
  SongPracticeRepository repo, {
  String id = kSeedPracticeId,
  String title = 'E2E Practice',
}) async {
  final now = DateTime.now();
  await repo.savePractice(
    SongPractice(
      id: id,
      title: title,
      versions: [
        TakeVersion(id: kSeedVersionId, name: 'Interpretation 1', createdAt: now),
      ],
      activeVersionId: kSeedVersionId,
      createdAt: now,
      updatedAt: now,
    ),
  );
}

/// Inserts a practice seeded with [takeCount] vocal takes, each backed by a real
/// WAV under [dir]. Returns the practice. Used by suites that need a populated
/// arrange/analysis/mixer screen without driving recording.
Future<SongPractice> seedPracticeWithTakes(
  SongPracticeRepository repo,
  Directory dir, {
  int takeCount = 2,
  String id = kSeedPracticeId,
}) async {
  final now = DateTime.now();
  final takes = <AudioTrack>[];
  for (var i = 0; i < takeCount; i++) {
    final wav = await writeSineWav(
      '${dir.path}/seed_take_$i.wav',
      freq: 180.0 + i * 60,
    );
    takes.add(
      AudioTrack(
        id: 'E2ETAKE${i}000000000000000000',
        kind: AudioTrackKind.vocal,
        displayName: 'Take ${i + 1}',
        audioFilePath: wav,
        durationMs: 600,
        createdAt: now,
        versionId: kSeedVersionId,
      ),
    );
  }
  final practice = SongPractice(
    id: id,
    title: 'E2E Practice',
    versions: [
      TakeVersion(id: kSeedVersionId, name: 'Interpretation 1', createdAt: now),
    ],
    activeVersionId: kSeedVersionId,
    vocalTracks: takes,
    createdAt: now,
    updatedAt: now,
  );
  final saved = await repo.savePractice(practice);
  return saved is Ok<SongPractice> ? saved.value : practice;
}

/// Inserts a technique with the fixed seed id so a START deep-link boots into it.
Future<void> seedTechnique(
  TechniqueRepository repo, {
  String id = kSeedTechniqueId,
  String name = 'E2E Technique',
}) async {
  final now = DateTime.now();
  await repo.saveTechnique(
    Technique(
      id: id,
      name: name,
      createdAt: now,
      updatedAt: now,
    ),
  );
}
