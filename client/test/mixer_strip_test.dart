import 'package:flutter/gestures.dart'
    show PointerDeviceKind, PointerScrollEvent, kPressTimeout, kSecondaryButton;
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:forui/forui.dart' show FIcons;
import 'package:vocara/data/services/app_settings_service.dart';
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/domain/models/audio_fx.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/fx_bus.dart';
import 'package:vocara/ui/song_practice_detail/view_model/mixer_controller.dart';
import 'package:vocara/ui/song_practice_detail/view_model/recording_controller.dart';
import 'package:vocara/ui/song_practice_detail/view_model/song_practice_detail_view_model.dart';
import 'package:vocara/ui/song_practice_detail/widgets/mixer_strip.dart';

/// Minimal fake mixer: only the members MixerStrip touches while building a
/// track strip + the master strip. `supportsEffects: false` (the default)
/// hides the mic-input and bus strips; [effects] turns them on for the bus
/// tests, which then need the host's audio settings and input meter too.
class _FakeMixer extends Fake implements MixerController {
  final List<double> trackVolumes = [];
  final List<double> masterVolumes = [];
  final List<String> deletedBuses = [];
  bool effects = false;
  List<FxBus> buses = const [];

  @override
  bool get supportsEffects => effects;

  @override
  List<FxBus> get fxBuses => buses;

  @override
  Future<void> deleteBus(String busId) async => deletedBuses.add(busId);

  @override
  double get masterVolume => 0.8;

  @override
  void setTrackVolumeLive(String trackId, double volume) =>
      trackVolumes.add(volume);

  int trackCommits = 0;

  @override
  Future<void> commitTrackVolume() async => trackCommits++;

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

  // The mic-input strip's dependencies (only built with mixer effects on).
  @override
  final AppSettingsService appSettings = AppSettingsService(
    storage: LocalStorageService(),
    enumerateOutputs: () => const [],
    enumerateInputs: () => const [],
  );

  @override
  final RecordingController recording = _FakeRecording();
}

class _FakeRecording extends Fake implements RecordingController {
  @override
  final ValueNotifier<double> inputLevel = ValueNotifier(0);
}

AudioTrack _track() => _trackN(1);

AudioTrack _trackN(int n) => AudioTrack(
  id: 't$n',
  kind: AudioTrackKind.vocal,
  displayName: 'Take $n',
  audioFilePath: '/x$n.wav',
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

  // Regression guard: the bus strip's trash icon (and its right-click "Delete
  // bus") deleted on the spot, while the timeline's bus lane asks first — and
  // a delete takes the bus's whole FX chain and every send into it, with no
  // undo.
  group('deleting a bus from the mixer asks first', () {
    Future<_FakeVm> pumpWithBus(WidgetTester tester) async {
      final vm = _FakeVm();
      vm.fakeMixer
        ..effects = true
        ..buses = [FxBus(id: 'b1', name: 'Lead Vox', fx: TrackFx.flat())];
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: SizedBox(
              height: 420,
              width: 900,
              child: MixerStrip(
                vm: vm,
                tracks: const [],
                onDeleteTrack: (_) {},
              ),
            ),
          ),
        ),
      );
      return vm;
    }

    testWidgets('the trash icon', (tester) async {
      final vm = await pumpWithBus(tester);
      await tester.tap(find.byIcon(FIcons.trash2));
      await tester.pumpAndSettle();
      expect(find.text('Delete "Lead Vox"?'), findsOneWidget);
      expect(vm.fakeMixer.deletedBuses, isEmpty);

      await tester.tap(find.text('Cancel'));
      await tester.pumpAndSettle();
      expect(vm.fakeMixer.deletedBuses, isEmpty);

      await tester.tap(find.byIcon(FIcons.trash2));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Delete'));
      await tester.pumpAndSettle();
      expect(vm.fakeMixer.deletedBuses, ['b1']);
      expect(tester.takeException(), isNull);
    });

    testWidgets('the right-click menu', (tester) async {
      final vm = await pumpWithBus(tester);
      await tester.tap(find.text('Lead Vox'), buttons: kSecondaryButton);
      await tester.pumpAndSettle();
      await tester.tap(find.text('Delete bus'));
      await tester.pumpAndSettle();
      expect(find.text('Delete "Lead Vox"?'), findsOneWidget);
      expect(vm.fakeMixer.deletedBuses, isEmpty);
      expect(tester.takeException(), isNull);
    });

    testWidgets('the right-click menu opens at the click', (tester) async {
      // It anchored below the full-height strip, opening at the bottom of the
      // mixer however high up the strip was clicked.
      await pumpWithBus(tester);
      final click = tester.getCenter(find.text('Lead Vox'));
      await tester.tapAt(click, buttons: kSecondaryButton);
      await tester.pumpAndSettle();
      final item = tester.getRect(find.text('Rename'));
      expect((item.top - click.dy).abs(), lessThan(40));
    });
  });

  testWidgets('a strip fader scrolled away mid-drag still saves its level', (
    tester,
  ) async {
    // A sideways wheel during the drag unmounts the strip from the lazy strip list, and a
    // disposed recognizer fires no end: the level the engine played was never saved.
    final vm = _FakeVm();
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: Align(
            alignment: Alignment.topLeft,
            child: SizedBox(
              height: 400,
              width: 600,
              child: MixerStrip(
                vm: vm,
                tracks: [for (var i = 0; i < 30; i++) _trackN(i)],
                onDeleteTrack: (_) {},
              ),
            ),
          ),
        ),
      ),
    );
    final g = await tester.startGesture(
      tester.getRect(_faders.first).center,
      kind: PointerDeviceKind.mouse,
    );
    await tester.pump();
    await g.moveBy(const Offset(0, -30));
    await tester.pump();
    await g.moveBy(const Offset(0, -10));
    await tester.pump();
    expect(vm.fakeMixer.trackVolumes, isNotEmpty);
    tester.binding.handlePointerEvent(
      PointerScrollEvent(
        position: const Offset(300, 200),
        kind: PointerDeviceKind.mouse,
        scrollDelta: const Offset(2500, 0),
      ),
    );
    await tester.pump();
    await tester.pump();
    expect(find.text('Take 0'), findsNothing);
    await g.up();
    await tester.pumpAndSettle();
    expect(vm.fakeMixer.trackCommits, 1);
  });

  Future<_FakeVm> pumpStrip(WidgetTester tester) async {
    final vm = _FakeVm();
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: Align(
            alignment: Alignment.topLeft,
            child: SizedBox(
              height: 400,
              width: 600,
              child: MixerStrip(
                vm: vm,
                tracks: [_trackN(0)],
                onDeleteTrack: (_) {},
              ),
            ),
          ),
        ),
      ),
    );
    return vm;
  }

  testWidgets('a held fader press that is cancelled still saves its level', (
    tester,
  ) async {
    // The press sets the level at tap-down; a pointer cancelled after it (a
    // dialog opening while the button is held) fired no tap-up, so the level
    // the engine played was never saved.
    final vm = await pumpStrip(tester);
    final g = await tester.startGesture(
      tester.getRect(_faders.first).center + const Offset(0, -20),
      kind: PointerDeviceKind.mouse,
    );
    await tester.pump(kPressTimeout + const Duration(milliseconds: 20));
    expect(vm.fakeMixer.trackVolumes, isNotEmpty);
    await g.cancel();
    await tester.pumpAndSettle();
    expect(vm.fakeMixer.trackCommits, 1);
  });

  testWidgets('a cancelled fader drag still saves its level', (tester) async {
    final vm = await pumpStrip(tester);
    final g = await tester.startGesture(
      tester.getRect(_faders.first).center,
      kind: PointerDeviceKind.mouse,
    );
    await tester.pump();
    await g.moveBy(const Offset(0, -30));
    await tester.pump();
    await g.moveBy(const Offset(0, -10));
    await tester.pump();
    expect(vm.fakeMixer.trackVolumes, isNotEmpty);
    await g.cancel();
    await tester.pumpAndSettle();
    expect(vm.fakeMixer.trackCommits, 1);
  });

  testWidgets('a held press that turns into a drag saves once', (tester) async {
    // The tap is cancelled when the drag wins, and saving there too wrote the
    // level mid-drag on top of the drag's own save at its end.
    final vm = await pumpStrip(tester);
    final g = await tester.startGesture(
      tester.getRect(_faders.first).center,
      kind: PointerDeviceKind.mouse,
    );
    await tester.pump(kPressTimeout + const Duration(milliseconds: 20));
    await g.moveBy(const Offset(0, -30));
    await tester.pump();
    await g.moveBy(const Offset(0, -10));
    await tester.pump();
    expect(vm.fakeMixer.trackCommits, 0, reason: 'mid-drag');
    await g.up();
    await tester.pumpAndSettle();
    expect(vm.fakeMixer.trackCommits, 1);
  });
}
