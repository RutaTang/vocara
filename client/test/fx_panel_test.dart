// The FX panel is the whole insert chain for a take OR a bus — nine plugins,
// each with a header toggle and (when on) its own editor — and it had no
// widget coverage at all.
//
// What matters here is the DISPATCH: the same panel drives a track through
// setTrackFxLive/reorderTrackInserts and a bus through setBusFxLive/
// reorderBusInserts, and reads its gain-reduction meter from a different
// source for each. A wire crossed between those two paths edits the wrong
// channel, which is invisible to every layer below.

import 'dart:typed_data';

import 'package:flutter/gestures.dart' show PointerDeviceKind, kPressTimeout;
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/domain/models/audio_fx.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/fx_bus.dart';
import 'package:vocara/domain/models/fx_presets.dart';
import 'package:vocara/ui/song_practice_detail/view_model/analysis_cache.dart';
import 'package:vocara/ui/song_practice_detail/view_model/mixer_controller.dart';
import 'package:vocara/ui/song_practice_detail/view_model/song_practice_detail_view_model.dart';
import 'package:vocara/ui/song_practice_detail/widgets/fx_panel.dart';

class _FakeMixer extends Fake implements MixerController {
  final trackFx = <TrackFx>[];
  final busFx = <(String, TrackFx)>[];
  final trackReorders = <(String, List<FxKind>)>[];
  final busReorders = <(String, List<FxKind>)>[];
  double trackGr = 0;
  double busGr = 0;
  int grPolls = 0;

  @override
  void setTrackFxLive(String trackId, TrackFx fx) => trackFx.add(fx);

  @override
  void setBusFxLive(String busId, TrackFx fx) => busFx.add((busId, fx));

  @override
  Future<void> reorderTrackInserts(String trackId, List<FxKind> order) async =>
      trackReorders.add((trackId, order));

  @override
  Future<void> reorderBusInserts(String busId, List<FxKind> order) async =>
      busReorders.add((busId, order));

  @override
  double gainReductionDb(String trackId) {
    grPolls++;
    return trackGr;
  }

  @override
  double busGainReductionDb(String busId) {
    grPolls++;
    return busGr;
  }
}

// A listenable: the EQ analyzer wakes on the analysis caches, where a newly
// computed input spectrum arrives.
class _FakeAnalysis extends Fake with ChangeNotifier implements AnalysisCache {
  int eqInputCalls = 0;

  /// The pre-EQ column under the playhead; null = no spectrum yet (the graph
  /// draws the response curve alone).
  ({Float32List col, double binHz})? input;

  @override
  ({Float32List col, double binHz})? eqInputColumn(String trackId) {
    eqInputCalls++;
    return input;
  }
}

class _FakeVm extends Fake implements SongPracticeDetailViewModel {
  final fakeMixer = _FakeMixer();
  final fakeAnalysis = _FakeAnalysis();
  int commits = 0;

  @override
  MixerController get mixer => fakeMixer;

  @override
  AnalysisCache get analysis => fakeAnalysis;

  @override
  final ValueNotifier<Duration> position = ValueNotifier(Duration.zero);

  @override
  Future<void> commitFx() async => commits++;
}

TrackFx _fxAllOff() => TrackFx.flat();

AudioTrack _track({TrackFx? fx}) => AudioTrack(
  id: 't1',
  kind: AudioTrackKind.vocal,
  displayName: 'Take 1',
  audioFilePath: '/x.wav',
  durationMs: 5000,
  createdAt: DateTime(2026),
  fx: fx,
);

FxBus _bus({TrackFx? fx}) =>
    FxBus(id: 'busA', name: 'Lead', fx: fx ?? _fxAllOff());

void main() {
  test('small rates and times keep a decimal in their readout', () {
    // The doubler's 0.6 Hz default read "1 Hz"; a 0.2 ms attack "0 ms".
    expect(fxValueLabel(0.6, 'Hz'), '0.6 Hz');
    expect(fxValueLabel(0.2, 'ms'), '0.2 ms');
    expect(fxValueLabel(120, 'Hz'), '120 Hz');
    expect(fxValueLabel(2500, 'Hz'), '2.5k');
    expect(fxValueLabel(40, 'ms'), '40 ms');
  });

  /// Pumps the panel at the real inspector width (LeftPanel's `_panelWidth`),
  /// tall enough that the whole ListView builds — an insert scrolled out of
  /// view is simply not there to tap.
  Future<_FakeVm> pump(
    WidgetTester tester, {
    AudioTrack? track,
    FxBus? bus,
  }) async {
    final vm = _FakeVm();
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: Center(
            child: SizedBox(
              width: 272,
              height: 4000,
              child: FxPanel(vm: vm, track: track, bus: bus),
            ),
          ),
        ),
      ),
    );
    return vm;
  }

  /// The header toggle for an insert, found by its title row.
  Finder toggleFor(String title) => find
      .descendant(
        of: find
            .ancestor(
              of: find.text(title.toUpperCase()),
              matching: find.byType(Row),
            )
            .first,
        matching: find.byType(GestureDetector),
      )
      .last;

  testWidgets('renders every insert header for a take with no fx yet', (
    tester,
  ) async {
    await pump(tester, track: _track());
    for (final title in const [
      'EQUALIZER',
      'COMPRESSOR',
      'DE-ESSER',
      'GATE',
      'DELAY',
      'SATURATION',
      'DOUBLER',
      'LIMITER',
      'REVERB',
    ]) {
      expect(find.text(title), findsOneWidget, reason: '$title header missing');
    }
    expect(tester.takeException(), isNull);
  });

  testWidgets('a track toggle edits the TRACK and commits', (tester) async {
    final vm = await pump(tester, track: _track());

    await tester.tap(toggleFor('Equalizer'));
    await tester.pump();

    expect(vm.fakeMixer.trackFx, hasLength(1));
    expect(vm.fakeMixer.trackFx.single.eqOn, isTrue);
    expect(vm.fakeMixer.busFx, isEmpty, reason: 'a take must not edit a bus');
    expect(vm.commits, 1, reason: 'a toggle is a committed edit, not a drag');
  });

  testWidgets('a bus toggle edits the BUS and commits', (tester) async {
    final vm = await pump(tester, bus: _bus());

    await tester.tap(toggleFor('Compressor'));
    await tester.pump();

    expect(vm.fakeMixer.busFx, hasLength(1));
    expect(vm.fakeMixer.busFx.single.$1, 'busA');
    expect(vm.fakeMixer.busFx.single.$2.compOn, isTrue);
    expect(vm.fakeMixer.trackFx, isEmpty, reason: 'a bus must not edit a take');
    expect(vm.commits, 1);
  });

  testWidgets('toggling an insert ON reveals its editor, OFF hides it', (
    tester,
  ) async {
    // The compressor's editor is the only place these labels appear.
    await pump(tester, track: _track(fx: _fxAllOff().copyWith(compOn: true)));
    expect(find.text('Ratio'), findsOneWidget);
    expect(find.text('Makeup'), findsOneWidget);

    await pump(tester, track: _track(fx: _fxAllOff()));
    expect(find.text('Ratio'), findsNothing);
    expect(find.text('Makeup'), findsNothing);
  });

  testWidgets('a held slider press that is cancelled still commits', (
    tester,
  ) async {
    // The press sets the value at tap-down; a pointer cancelled after it (a
    // dialog opening while the button is held) fired no tap-up, so the value
    // the engine played was never saved.
    final vm = await pump(
      tester,
      track: _track(fx: _fxAllOff().copyWith(compOn: true)),
    );
    final slider = find
        .byWidgetPredicate((w) => w.runtimeType.toString() == '_FxSlider')
        .first;
    final g = await tester.startGesture(
      tester.getRect(slider).center,
      kind: PointerDeviceKind.mouse,
    );
    await tester.pump(kPressTimeout + const Duration(milliseconds: 20));
    await g.cancel();
    await tester.pump();
    expect(vm.commits, 1);
  });

  Finder byTypeName(String name) =>
      find.byWidgetPredicate((w) => w.runtimeType.toString() == name).first;

  testWidgets('a held slider press that turns into a pan commits once', (
    tester,
  ) async {
    final vm = await pump(
      tester,
      track: _track(fx: _fxAllOff().copyWith(compOn: true)),
    );
    final g = await tester.startGesture(
      tester.getRect(byTypeName('_FxSlider')).center,
      kind: PointerDeviceKind.mouse,
    );
    await tester.pump(kPressTimeout + const Duration(milliseconds: 20));
    await g.moveBy(const Offset(30, 0));
    await tester.pump();
    await g.moveBy(const Offset(10, 0));
    await tester.pump();
    expect(vm.commits, 0, reason: 'mid-pan');
    await g.up();
    await tester.pump();
    expect(vm.commits, 1);
  });

  testWidgets('a touch press that slips past the tap slop commits once', (
    tester,
  ) async {
    final vm = await pump(
      tester,
      track: _track(fx: _fxAllOff().copyWith(compOn: true)),
    );
    final g = await tester.startGesture(
      tester.getRect(byTypeName('_FxSlider')).center,
      kind: PointerDeviceKind.touch,
    );
    await tester.pump(kPressTimeout + const Duration(milliseconds: 20));
    await g.moveBy(const Offset(25, 0)); // past the tap's slop, not the pan's
    await tester.pump();
    await g.moveBy(const Offset(20, 0)); // the pan wins
    await tester.pump();
    await g.moveBy(const Offset(10, 0)); // and moves the value
    await tester.pump();
    await g.up();
    await tester.pump();
    expect(vm.commits, 1);
  });

  testWidgets('a two-finger trackpad swipe over the editors moves nothing', (
    tester,
  ) async {
    // The swipe scrolls the panel; a pan recognizer takes a trackpad pan by
    // default, so it also moved (and saved) the slider or EQ band it crossed.
    final vm = await pump(
      tester,
      track: _track(fx: _fxAllOff().copyWith(compOn: true, eqOn: true)),
    );
    for (final target in [byTypeName('_FxSlider'), byTypeName('_EqGraph')]) {
      final at = tester.getRect(target).center;
      final g = await tester.createGesture(kind: PointerDeviceKind.trackpad);
      await g.panZoomStart(at);
      await tester.pump();
      await g.panZoomUpdate(at, pan: const Offset(30, -20));
      await tester.pump();
      await g.panZoomUpdate(at, pan: const Offset(60, -40));
      await tester.pump();
      await g.panZoomEnd();
      await tester.pump();
    }
    expect(vm.fakeMixer.trackFx, isEmpty);
    expect(vm.commits, 0);
  });

  testWidgets(
    'a second finger lifting off a held slider commits nothing early',
    (tester) async {
      final vm = await pump(
        tester,
        track: _track(fx: _fxAllOff().copyWith(compOn: true)),
      );
      final at = tester.getRect(byTypeName('_FxSlider')).center;
      final first = await tester.startGesture(
        at,
        kind: PointerDeviceKind.touch,
        pointer: 1,
      );
      await tester.pump(kPressTimeout + const Duration(milliseconds: 20));
      final second = await tester.startGesture(
        at + const Offset(20, 0),
        kind: PointerDeviceKind.touch,
        pointer: 2,
      );
      await tester.pump();
      await second.up();
      await tester.pump();
      expect(vm.commits, 0, reason: 'finger 1 still holds');
      await first.up();
      await tester.pump();
      expect(vm.commits, 1);
    },
  );

  testWidgets('a slider unmounted mid-pan still commits', (tester) async {
    // Unmounted mid-gesture (the selection changed, the inspector collapsed),
    // a disposed recognizer fires no end: the value already live in the model
    // was never written.
    final vm = await pump(
      tester,
      track: _track(fx: _fxAllOff().copyWith(compOn: true)),
    );
    final g = await tester.startGesture(
      tester.getRect(byTypeName('_FxSlider')).center,
      kind: PointerDeviceKind.mouse,
    );
    await tester.pump();
    await g.moveBy(const Offset(30, 0));
    await tester.pump();
    await g.moveBy(const Offset(10, 0));
    await tester.pump();
    await tester.pumpWidget(const SizedBox());
    await g.up();
    await tester.pump();
    expect(vm.commits, 1);
  });

  testWidgets('an EQ band unmounted mid-drag still commits', (tester) async {
    final vm = await pump(
      tester,
      track: _track(fx: _fxAllOff().copyWith(eqOn: true)),
    );
    final g = await tester.startGesture(
      tester.getRect(byTypeName('_EqGraph')).center,
      kind: PointerDeviceKind.mouse,
    );
    await tester.pump();
    await g.moveBy(const Offset(30, -10));
    await tester.pump();
    await g.moveBy(const Offset(10, -5));
    await tester.pump();
    await tester.pumpWidget(const SizedBox());
    await g.up();
    await tester.pump();
    expect(vm.commits, 1);
  });

  testWidgets('every insert editor renders with its plugin on', (tester) async {
    // All nine editors laid out at once: an out-of-range clamp or a bad
    // constraint in any of them surfaces as a thrown exception here.
    final fx = _fxAllOff().copyWith(
      eqOn: true,
      compOn: true,
      deEsserOn: true,
      gateOn: true,
      delayOn: true,
      saturationOn: true,
      doublerOn: true,
      limiterOn: true,
    );
    await pump(tester, track: _track(fx: fx));
    expect(tester.takeException(), isNull);
  });

  testWidgets('the EQ graph asks the analysis cache for a take, never for a '
      'bus', (tester) async {
    final withTake = await pump(
      tester,
      track: _track(fx: _fxAllOff().copyWith(eqOn: true)),
    );
    // The analyzer polls on a Ticker, not in build, so time has to move.
    await tester.pump(const Duration(milliseconds: 32));
    expect(
      withTake.fakeAnalysis.eqInputCalls,
      greaterThan(0),
      reason: 'a take has a pre-EQ input spectrum',
    );

    final withBus = await pump(
      tester,
      bus: _bus(fx: _fxAllOff().copyWith(eqOn: true)),
    );
    await tester.pump(const Duration(milliseconds: 32));
    expect(
      withBus.fakeAnalysis.eqInputCalls,
      0,
      reason: 'a bus is a sum of routed takes — it has no input column',
    );
  });

  testWidgets('the gain-reduction meter reads the bus source for a bus', (
    tester,
  ) async {
    final vm = await pump(
      tester,
      bus: _bus(fx: _fxAllOff().copyWith(compOn: true)),
    );
    vm.fakeMixer.busGr = 6;
    vm.fakeMixer.trackGr = 99; // must not be read
    await tester.pump(const Duration(milliseconds: 100));
    expect(tester.takeException(), isNull);
  });

  // Regression guard: a scroll-wheel Q edit persists on a 400 ms debounce, and
  // dispose CANCELLED that pending commit — so changing the selection,
  // collapsing the inspector or leaving within 400 ms of the last wheel tick
  // kept the Q live in memory and never wrote it.
  testWidgets('a wheel Q edit still commits when the EQ goes away inside the '
      'debounce', (tester) async {
    final vm = await pump(
      tester,
      track: _track(fx: _fxAllOff().copyWith(eqOn: true)),
    );
    final graph = find.byWidgetPredicate(
      (w) => w.runtimeType.toString() == '_EqGraph',
    );
    final mouse = TestPointer(1, PointerDeviceKind.mouse);
    await tester.sendEventToBinding(mouse.hover(tester.getCenter(graph)));
    await tester.sendEventToBinding(mouse.scroll(const Offset(0, -40)));
    await tester.pump();
    expect(vm.fakeMixer.trackFx, hasLength(1), reason: 'the Q edit is live');
    expect(vm.commits, 0, reason: 'debounced, not yet written');

    // Selection moves on before the debounce fires.
    await tester.pumpWidget(const SizedBox());
    expect(vm.commits, 1, reason: 'the pending Q must be flushed, not dropped');

    await tester.pump(const Duration(milliseconds: 500));
    expect(vm.commits, 1, reason: 'flushed once — the timer is gone');
    expect(tester.takeException(), isNull);
  });

  testWidgets('choosing a preset replaces the chain but keeps the order', (
    tester,
  ) async {
    final custom = [FxKind.comp, FxKind.eq, ...FxKind.values.skip(2)];
    final vm = await pump(
      tester,
      track: _track(fx: _fxAllOff().copyWith(order: custom, sendDb: -12)),
    );

    await tester.tap(find.text('Choose…'));
    await tester.pumpAndSettle();
    final preset = FxPresets.vocal.first;
    await tester.tap(find.text(preset.name).last);
    await tester.pumpAndSettle();

    expect(vm.fakeMixer.trackFx, hasLength(1));
    expect(
      vm.fakeMixer.trackFx.single.order,
      custom,
      reason: "a preset is tone, not the user's plugin order",
    );
    // The legacy reverb send nothing else can clear goes with the chain.
    expect(vm.fakeMixer.trackFx.single.sendDb, kSendOffDb);
    expect(vm.commits, 1);
  });

  // The EQ analyzer ticked at the display rate, and the GR meter rebuilt at
  // 20 Hz, for as long as they were on screen — a paused transport never let
  // the app go idle. Both now stop once nothing moves, and the playhead (or a
  // rebuild) starts them again.
  testWidgets('the EQ analyzer stops ticking once settled and the playhead '
      'wakes it', (tester) async {
    final vm = await pump(
      tester,
      track: _track(fx: _fxAllOff().copyWith(eqOn: true)),
    );
    // A real input column, so the ballistics have something to settle on.
    vm.fakeAnalysis.input = (
      col: Float32List.fromList([for (var b = 0; b < 1025; b++) -40.0 - b % 7]),
      binHz: 48000 / 2048,
    );
    Future<int> settle() async {
      var frames = 0;
      while (tester.binding.hasScheduledFrame && frames < 600) {
        await tester.pump(const Duration(milliseconds: 16));
        frames++;
      }
      return frames;
    }

    final first = await settle();
    expect(tester.binding.hasScheduledFrame, isFalse, reason: 'idle');
    expect(first, lessThan(600));
    final calls = vm.fakeAnalysis.eqInputCalls;
    await tester.pump(const Duration(seconds: 1));
    expect(
      vm.fakeAnalysis.eqInputCalls,
      calls,
      reason: 'no polling while idle',
    );

    // The playhead moved: the analyzer runs again, then idles again.
    vm.position.value = const Duration(milliseconds: 500);
    expect(tester.binding.hasScheduledFrame, isTrue);
    await settle();
    expect(vm.fakeAnalysis.eqInputCalls, greaterThan(calls));
    expect(tester.binding.hasScheduledFrame, isFalse);
  });

  // The inspector keeps its FX panel across every view-model notification
  // that leaves the take alone (a fader or clip drag elsewhere), so a rebuild
  // no longer brings the analyzer the input spectrum that has just been
  // computed: the analysis caches announcing it must wake it on their own,
  // or a paused take's EQ stays blank until the playhead moves.
  testWidgets('an input spectrum computed while the EQ analyzer idles wakes '
      'it without a rebuild', (tester) async {
    final vm = await pump(
      tester,
      track: _track(fx: _fxAllOff().copyWith(eqOn: true)),
    );
    var frames = 0;
    while (tester.binding.hasScheduledFrame && frames < 600) {
      await tester.pump(const Duration(milliseconds: 16));
      frames++;
    }
    expect(tester.binding.hasScheduledFrame, isFalse, reason: 'idle');
    final calls = vm.fakeAnalysis.eqInputCalls;

    vm.fakeAnalysis.input = (
      col: Float32List.fromList(List.filled(1025, -20.0)),
      binHz: 48000 / 2048,
    );
    vm.fakeAnalysis.notifyListeners();
    expect(tester.binding.hasScheduledFrame, isTrue, reason: 'woken');
    await tester.pump(const Duration(milliseconds: 16));
    expect(vm.fakeAnalysis.eqInputCalls, greaterThan(calls));
  });

  testWidgets('the EQ analyzer decays to the floor before it idles', (
    tester,
  ) async {
    final vm = await pump(
      tester,
      track: _track(fx: _fxAllOff().copyWith(eqOn: true)),
    );
    vm.fakeAnalysis.input = (
      col: Float32List.fromList(List.filled(1025, -10.0)),
      binHz: 48000 / 2048,
    );
    await tester.pump(const Duration(milliseconds: 16));
    await tester.pump(const Duration(milliseconds: 16));
    // The input goes away (playhead left the clip): the release takes a few
    // seconds at 34 dB/s, and the ticker must run all of it.
    vm.fakeAnalysis.input = null;
    vm.position.value = const Duration(milliseconds: 9000);
    var frames = 0;
    while (tester.binding.hasScheduledFrame && frames < 1000) {
      await tester.pump(const Duration(milliseconds: 16));
      frames++;
    }
    expect(frames, greaterThan(100), reason: 'the full release is animated');
    expect(tester.binding.hasScheduledFrame, isFalse);
  });

  testWidgets('the gain-reduction meter stops polling once still and the '
      'playhead wakes it', (tester) async {
    final vm = await pump(
      tester,
      track: _track(fx: _fxAllOff().copyWith(compOn: true)),
    );
    await tester.pump(const Duration(seconds: 2));
    final polls = vm.fakeMixer.grPolls;
    expect(polls, greaterThan(0));
    await tester.pump(const Duration(seconds: 2));
    expect(vm.fakeMixer.grPolls, polls, reason: 'idle with nothing playing');

    // Audio through the compressor moves the playhead: polling resumes and
    // the meter shows the reduction.
    vm.fakeMixer.trackGr = 6;
    vm.position.value = const Duration(milliseconds: 100);
    await tester.pump(const Duration(milliseconds: 120));
    expect(vm.fakeMixer.grPolls, greaterThan(polls));
    expect(find.text('-6.0'), findsOneWidget);
    // Held still again, it goes idle again.
    await tester.pump(const Duration(seconds: 2));
    final later = vm.fakeMixer.grPolls;
    await tester.pump(const Duration(seconds: 1));
    expect(vm.fakeMixer.grPolls, later);
    expect(find.text('-6.0'), findsOneWidget);
  });
}
