// The inline example players own native media_kit players. These tests swap
// in a fake (a widget test cannot initialize media_kit) to pin that every
// player built is released, and that a clip whose file is not on this device
// renders as unavailable instead of throwing.

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:media_kit/media_kit.dart';
import 'package:vocara/ui/technique_library/widgets/media/technique_audio_player.dart';
import 'package:vocara/ui/technique_library/widgets/media/technique_video_player.dart';

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
}
