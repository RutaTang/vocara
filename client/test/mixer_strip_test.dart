import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/fx_bus.dart';
import 'package:vocara/ui/song_practice_detail/view_model/mixer_controller.dart';
import 'package:vocara/ui/song_practice_detail/view_model/song_practice_detail_view_model.dart';
import 'package:vocara/ui/song_practice_detail/widgets/mixer_strip.dart';

/// Minimal fake mixer: only the members MixerStrip touches while building a
/// track strip + the master strip. `supportsEffects: false` hides the
/// mic-input and bus strips, which would drag in audio settings / engine state.
class _FakeMixer extends Fake implements MixerController {
  final List<double> trackVolumes = [];
  final List<double> masterVolumes = [];

  @override
  bool get supportsEffects => false;

  @override
  List<FxBus> get fxBuses => const [];

  @override
  double get masterVolume => 0.8;

  @override
  void setTrackVolumeLive(String trackId, double volume) =>
      trackVolumes.add(volume);

  @override
  Future<void> commitTrackVolume() async {}

  @override
  void setMasterVolumeLive(double gain) => masterVolumes.add(gain);

  @override
  Future<void> commitMasterVolume() async {}
}

/// Minimal fake host: selection (a core VM concern) plus the [mixer] the
/// strip's level/routing controls talk to directly.
class _FakeVm extends Fake implements SongPracticeDetailViewModel {
  final _FakeMixer fakeMixer = _FakeMixer();

  @override
  MixerController get mixer => fakeMixer;

  @override
  String? get selectedTrackId => null;

  @override
  String? get selectedBusId => null;
}

AudioTrack _track() => AudioTrack(
  id: 't1',
  kind: AudioTrackKind.vocal,
  displayName: 'Take 1',
  audioFilePath: '/x.wav',
  durationMs: 5000,
  createdAt: DateTime(2026),
);

/// Pumps the mixer inside a fixed-height box, like the resizable bottom dock.
Future<void> _pumpMixer(
  WidgetTester tester,
  _FakeVm vm, {
  required double height,
}) {
  return tester.pumpWidget(
    MaterialApp(
      home: Scaffold(
        body: Align(
          alignment: Alignment.topLeft,
          child: SizedBox(
            height: height,
            width: 600,
            child: MixerStrip(
              vm: vm,
              tracks: [_track()],
              onDeleteTrack: (_) {},
            ),
          ),
        ),
      ),
    ),
  );
}

/// The channel-strip vertical faders (they are the only widgets using the
/// resize-up-down cursor). First match = the track strip's fader.
final Finder _faders = find.byWidgetPredicate(
  (w) => w is MouseRegion && w.cursor == SystemMouseCursors.resizeUpDown,
);

void main() {
  // Regression guard: an earlier fix wrapped the strip columns in
  // IntrinsicHeight, but the faders are LayoutBuilder-based and LayoutBuilder
  // does not support intrinsic measurement — every layout threw in debug
  // builds. These tests lay the mixer out at a normal AND a squeezed height
  // and fail on ANY exception, which is exactly what that regression missed.
  testWidgets('mixer lays out and the fader works at a normal dock height', (
    tester,
  ) async {
    final vm = _FakeVm();
    await _pumpMixer(tester, vm, height: 400);
    expect(tester.takeException(), isNull);

    // Tall enough: the plain column branch — no per-strip scroll view.
    expect(find.byType(SingleChildScrollView), findsNothing);

    // The Expanded fader must be live: dragging it reports volume ticks.
    await tester.drag(_faders.first, const Offset(0, -40));
    await tester.pump();
    expect(tester.takeException(), isNull);
    expect(vm.fakeMixer.trackVolumes, isNotEmpty);
  });

  testWidgets('squeezed mixer scrolls per strip and the fader still works', (
    tester,
  ) async {
    final vm = _FakeVm();
    // 270 − ListView padding (24) − strip border+padding (26) = 220 inner px:
    // below the min-content threshold, so the scrolling fallback kicks in.
    await _pumpMixer(tester, vm, height: 270);
    expect(tester.takeException(), isNull);

    // Squeezed: each strip (track + master) swapped to its scroll fallback.
    expect(find.byType(SingleChildScrollView), findsNWidgets(2));

    // The fixed-height fader inside the scroll branch must be interactive
    // (this exercises the LayoutBuilder fader inside the scrolling column).
    await tester.drag(_faders.first, const Offset(0, -30));
    await tester.pump();
    expect(tester.takeException(), isNull);
    expect(vm.fakeMixer.trackVolumes, isNotEmpty);
  });

  testWidgets('severely squeezed mixer still lays out without exceptions', (
    tester,
  ) async {
    final vm = _FakeVm();
    await _pumpMixer(tester, vm, height: 80);
    expect(tester.takeException(), isNull);
    expect(find.byType(SingleChildScrollView), findsNWidgets(2));
  });
}
