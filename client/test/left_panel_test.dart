// The inspector's channel-strip footer: aux sends, output routing, pan, the
// fader and mute/solo — for a selected take AND for a selected bus, through
// the same widgets but different view-model methods. It had two covered lines
// out of 526.
//
// Two things are worth pinning here. First the DISPATCH, as in fx_panel_test:
// a bus and a take share this UI and must never reach each other's methods.
// Second the send menus' OFFER SETS: "add" must not offer a bus a slot already
// feeds, and "retarget" must offer the slot's own current target plus the
// unused ones. Two sends into one bus feed it twice, so the bus hears the sum
// and neither fader reads what you hear — the view model now refuses that, and
// this is the half of the guard the user actually sees.

import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/domain/models/audio_fx.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/fx_bus.dart';
import 'package:vocara/domain/models/take_version.dart';
import 'package:vocara/domain/models/voice_metrics.dart';
import 'package:vocara/ui/song_practice_detail/view_model/analysis_cache.dart';
import 'package:vocara/ui/song_practice_detail/view_model/interpretations_controller.dart';
import 'package:vocara/ui/song_practice_detail/view_model/mixer_controller.dart';
import 'package:vocara/ui/song_practice_detail/view_model/song_practice_detail_view_model.dart';
import 'package:vocara/ui/song_practice_detail/widgets/left_panel.dart';

class _FakeMixer extends Fake implements MixerController {
  _FakeMixer(this.buses);

  final List<FxBus> buses;

  final trackSendAdds = <(String, String)>[];
  final busSendAdds = <(String, String)>[];
  final trackSendRemoves = <(String, int)>[];
  final trackRetargets = <(String, int, String)>[];
  final trackBusRoutes = <(String, String?)>[];
  final busOutputRoutes = <(String, String?)>[];
  final trackPans = <(String, double)>[];
  final busPans = <(String, double)>[];
  final trackMutes = <String>[];
  final busMutes = <String>[];
  final trackSolos = <String>[];
  final busSolos = <String>[];

  @override
  bool get supportsEffects => true;

  @override
  List<FxBus> get fxBuses => buses;

  @override
  Future<void> addTrackSend(String trackId, String busId) async =>
      trackSendAdds.add((trackId, busId));

  @override
  Future<void> addBusSend(String busId, String targetBusId) async =>
      busSendAdds.add((busId, targetBusId));

  @override
  Future<void> removeTrackSend(String trackId, int slot) async =>
      trackSendRemoves.add((trackId, slot));

  @override
  Future<void> setTrackSendTarget(
    String trackId,
    int slot,
    String busId,
  ) async => trackRetargets.add((trackId, slot, busId));

  @override
  void setTrackSendLevelLive(String trackId, int slot, double levelDb) {}

  @override
  void setBusSendLevelLive(String busId, int slot, double levelDb) {}

  @override
  Future<void> setTrackBus(String trackId, String? busId) async =>
      trackBusRoutes.add((trackId, busId));

  @override
  Future<void> setBusOutput(String busId, String? outputBusId) async =>
      busOutputRoutes.add((busId, outputBusId));

  @override
  void setTrackPanLive(String trackId, double pan) =>
      trackPans.add((trackId, pan));

  @override
  void setBusPanLive(String busId, double pan) => busPans.add((busId, pan));

  @override
  void setTrackVolumeLive(String trackId, double volume) {}

  @override
  void setBusVolumeLive(String busId, double volume) {}

  @override
  Future<void> commitTrackVolume() async {}

  @override
  Future<void> toggleMute(String trackId) async => trackMutes.add(trackId);

  @override
  Future<void> toggleSolo(String trackId) async => trackSolos.add(trackId);

  @override
  Future<void> toggleBusMute(String busId) async => busMutes.add(busId);

  @override
  Future<void> toggleBusSolo(String busId) async => busSolos.add(busId);

  @override
  double gainReductionDb(String trackId) => 0;

  @override
  double busGainReductionDb(String busId) => 0;
}

// A listenable: the FX panel's EQ analyzer wakes on the analysis caches.
class _FakeAnalysis extends Fake with ChangeNotifier implements AnalysisCache {
  @override
  ({Float32List col, double binHz})? eqInputColumn(String trackId) => null;

  @override
  bool metricsLoadingFor(String trackId) => false;

  @override
  bool get selectedMetricsUnavailable => false;

  @override
  Future<void> ensureSelectedSpectrogram() async {}

  @override
  void showDockTab(DockTab tab) {}
}

class _FakeVersions extends Fake implements InterpretationsController {
  _FakeVersions(this.takes);

  final List<AudioTrack> takes;

  @override
  List<AudioTrack> get vocalTakes => takes;

  @override
  TakeVersion? get active => TakeVersion(
    id: 'v1',
    name: 'Interpretation 1',
    createdAt: DateTime(2026),
  );
}

class _FakeVm extends Fake implements SongPracticeDetailViewModel {
  _FakeVm({required this.fakeMixer, this.track, this.bus})
    : fakeVersions = _FakeVersions([?track]);

  final _FakeVersions fakeVersions;

  @override
  InterpretationsController get versions => fakeVersions;

  final _FakeMixer fakeMixer;
  final AudioTrack? track;
  final FxBus? bus;
  final fakeAnalysis = _FakeAnalysis();

  @override
  MixerController get mixer => fakeMixer;

  @override
  AnalysisCache get analysis => fakeAnalysis;

  @override
  bool get inspectorOpen => true;

  @override
  void toggleInspectorPanel() {}

  @override
  AudioTrack? get selectedTrack => track;

  @override
  FxBus? get selectedBus => bus;

  @override
  Future<void> commitFx() async {}

  // The FX panel's idle analyzer and meter wake on the playhead.
  @override
  final ValueNotifier<Duration> position = ValueNotifier(Duration.zero);

  @override
  bool get spectrogramLoading => false;

  @override
  VoiceMetrics? get selectedMetrics => null;

  @override
  VoiceProfile? get selectedProfile => null;

  @override
  RegisterMap? get selectedRegister => null;

  @override
  Ltas? get selectedLtas => null;
}

AudioTrack _vocal({
  String id = 't1',
  List<TrackSend> sends = const [],
  String? busId,
  TrackFx? fx,
}) => AudioTrack(
  id: id,
  kind: AudioTrackKind.vocal,
  displayName: 'Take 1',
  audioFilePath: '/x.wav',
  durationMs: 5000,
  createdAt: DateTime(2026),
  sends: sends,
  busId: busId,
  fx: fx,
);

FxBus _bus(String id, String name) =>
    FxBus(id: id, name: name, fx: TrackFx.flat());

void main() {
  setUp(() {
    // The inspector pins its channel-strip footer only above a minimum height;
    // give it a real window rather than the 800x600 default, so these tests
    // exercise the pinned layout the app actually ships.
    final view =
        TestWidgetsFlutterBinding.instance.platformDispatcher.views.first;
    view.physicalSize = const Size(1440, 1200);
    view.devicePixelRatio = 1.0;
    addTearDown(() {
      view.resetPhysicalSize();
      view.resetDevicePixelRatio();
    });
  });

  Future<_FakeVm> pump(
    WidgetTester tester, {
    AudioTrack? track,
    FxBus? bus,
    List<FxBus> buses = const [],
    double? height,
  }) async {
    final vm = _FakeVm(fakeMixer: _FakeMixer(buses), track: track, bus: bus);
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: Align(
            alignment: Alignment.topLeft,
            child: SizedBox(
              height: height,
              child: Row(children: [LeftPanel(vm: vm)]),
            ),
          ),
        ),
      ),
    );
    return vm;
  }

  testWidgets('nothing selected shows the empty state', (tester) async {
    await pump(tester);
    expect(find.text('Select a take or bus to inspect it.'), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  // A short window with the dock open (or dragged tall) leaves the inspector
  // a sliver of height. The take strip scrolls there; the bus strip kept its
  // pinned layout, overflowed, and pushed its fader, mute/solo and pan under
  // the dock where nothing could reach them.
  group('a short inspector', () {
    testWidgets('a bus strip scrolls instead of overflowing', (tester) async {
      final buses = [_bus('a', 'Reverb'), _bus('b', 'Delay')];
      final vm = await pump(
        tester,
        bus: buses.first,
        buses: buses,
        height: 200,
      );
      expect(tester.takeException(), isNull);

      await tester.ensureVisible(find.text('M'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('M'));
      await tester.pump();
      expect(vm.fakeMixer.busMutes, ['a']);
    });

    testWidgets('a take strip scrolls too', (tester) async {
      final vm = await pump(
        tester,
        track: _vocal(),
        buses: [_bus('a', 'Reverb')],
        height: 200,
      );
      expect(tester.takeException(), isNull);

      await tester.ensureVisible(find.text('M'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('M'));
      await tester.pump();
      expect(vm.fakeMixer.trackMutes, ['t1']);
    });
  });

  testWidgets('a selected take renders its channel strip', (tester) async {
    await pump(tester, track: _vocal(), buses: [_bus('a', 'Reverb')]);
    expect(find.text('OUTPUT'), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  group('aux sends', () {
    testWidgets('the add menu offers only buses no slot already feeds', (
      tester,
    ) async {
      final vm = await pump(
        tester,
        track: _vocal(sends: const [TrackSend(busId: 'a')]),
        buses: [_bus('a', 'Reverb'), _bus('b', 'Delay'), _bus('c', 'Room')],
      );

      // Slot 0's chip already shows "Reverb", so count occurrences rather
      // than absence: if the menu offered it too there would be a second one.
      expect(find.text('Reverb'), findsOneWidget);
      await tester.tap(find.text('Send'));
      await tester.pumpAndSettle();

      expect(
        find.text('Reverb'),
        findsOneWidget,
        reason: 'still only the chip — bus a is already fed by slot 0',
      );
      expect(find.text('Delay'), findsOneWidget);
      expect(find.text('Room'), findsOneWidget);

      await tester.tap(find.text('Delay'));
      await tester.pumpAndSettle();
      expect(vm.fakeMixer.trackSendAdds, [('t1', 'b')]);
    });

    testWidgets('the retarget menu offers this slot\'s own target plus the '
        'unused ones', (tester) async {
      final vm = await pump(
        tester,
        track: _vocal(
          sends: const [
            TrackSend(busId: 'a'),
            TrackSend(busId: 'b'),
          ],
        ),
        buses: [_bus('a', 'Reverb'), _bus('b', 'Delay'), _bus('c', 'Room')],
      );

      // Slot 0's chip carries its current target's name; slot 1's carries
      // "Delay". Nothing else is on screen yet.
      expect(find.text('Reverb'), findsOneWidget);
      expect(find.text('Delay'), findsOneWidget);
      expect(find.text('Room'), findsNothing);

      await tester.tap(find.text('Reverb'));
      await tester.pumpAndSettle();

      expect(
        find.text('Reverb'),
        findsNWidgets(2),
        reason: 'the chip plus its own target as an option',
      );
      expect(
        find.text('Delay'),
        findsOneWidget,
        reason:
            "still only slot 1's chip — a bus another slot feeds is "
            'never offered',
      );
      expect(find.text('Room'), findsOneWidget, reason: 'unused, so offered');

      await tester.tap(find.text('Room'));
      await tester.pumpAndSettle();
      expect(vm.fakeMixer.trackRetargets, [('t1', 0, 'c')]);
    });

    testWidgets('removing a send passes the slot index', (tester) async {
      final vm = await pump(
        tester,
        track: _vocal(
          sends: const [
            TrackSend(busId: 'a'),
            TrackSend(busId: 'b'),
          ],
        ),
        buses: [_bus('a', 'Reverb'), _bus('b', 'Delay')],
      );

      // One remove affordance per slot, in slot order.
      final removes = find.byIcon(Icons.close);
      expect(removes, findsNWidgets(2));
      await tester.tap(removes.last);
      await tester.pump();
      expect(vm.fakeMixer.trackSendRemoves, [('t1', 1)]);
    });

    testWidgets('the add affordance disappears at the send cap', (
      tester,
    ) async {
      final buses = [for (var i = 0; i < kMaxSends; i++) _bus('b$i', 'Bus $i')];
      await pump(
        tester,
        track: _vocal(sends: [for (final b in buses) TrackSend(busId: b.id)]),
        buses: buses,
      );
      expect(find.text('Send'), findsNothing);
    });
  });

  group('take vs bus dispatch', () {
    testWidgets('mute and solo on a take reach the TRACK methods', (
      tester,
    ) async {
      final vm = await pump(tester, track: _vocal());
      await tester.tap(find.text('M'));
      await tester.pump();
      await tester.tap(find.text('S'));
      await tester.pump();

      expect(vm.fakeMixer.trackMutes, ['t1']);
      expect(vm.fakeMixer.trackSolos, ['t1']);
      expect(vm.fakeMixer.busMutes, isEmpty);
      expect(vm.fakeMixer.busSolos, isEmpty);
    });

    testWidgets('mute and solo on a bus reach the BUS methods', (tester) async {
      final vm = await pump(
        tester,
        bus: _bus('a', 'Reverb'),
        buses: [_bus('a', 'Reverb')],
      );
      await tester.tap(find.text('M'));
      await tester.pump();
      await tester.tap(find.text('S'));
      await tester.pump();

      expect(vm.fakeMixer.busMutes, ['a']);
      expect(vm.fakeMixer.busSolos, ['a']);
      expect(vm.fakeMixer.trackMutes, isEmpty);
      expect(vm.fakeMixer.trackSolos, isEmpty);
    });
  });

  group('menu placement', () {
    // The send chips and the bus OUTPUT chip opened their menus from an
    // ANCESTOR's context, so the menu dropped below the whole sends section /
    // footer (or at its left edge) instead of under the chip that was clicked.
    testWidgets('the add-send menu opens under its chip', (tester) async {
      await pump(
        tester,
        track: _vocal(
          sends: const [
            TrackSend(busId: 'a'),
            TrackSend(busId: 'b'),
          ],
        ),
        buses: [_bus('a', 'Reverb'), _bus('b', 'Delay'), _bus('c', 'Room')],
      );
      final chip = tester.getRect(find.text('Send'));
      await tester.tap(find.text('Send'));
      await tester.pumpAndSettle();
      final item = tester.getRect(find.text('Room'));
      expect(item.top - chip.bottom, lessThan(30));
    });

    testWidgets("a bus's OUTPUT menu opens under its chip", (tester) async {
      await pump(
        tester,
        bus: _bus('a', 'Reverb'),
        buses: [_bus('a', 'Reverb'), _bus('b', 'Delay')],
      );
      final chip = tester.getRect(find.text('Stereo Out'));
      await tester.tap(find.text('Stereo Out'));
      await tester.pumpAndSettle();
      // The menu's first row repeats the chip's label.
      final item = tester.getRect(find.text('Stereo Out').last);
      expect((item.left - chip.left).abs(), lessThan(30));
      expect(item.top - chip.bottom, lessThan(30));
    });
  });

  group('output routing', () {
    testWidgets('a take can be routed to a bus and back to the master', (
      tester,
    ) async {
      final vm = await pump(
        tester,
        track: _vocal(),
        buses: [_bus('a', 'Reverb')],
      );

      await tester.tap(find.text('Stereo Out'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Reverb').last);
      await tester.pumpAndSettle();
      expect(vm.fakeMixer.trackBusRoutes, [('t1', 'a')]);
    });

    // A bus name has no length cap, and the chip sat after a Spacer with no
    // bound: a long name ran the chip past the inspector's edge.
    const long = 'A return bus with a name well past the chip';

    void expectInsidePanel(WidgetTester tester) {
      expect(tester.takeException(), isNull);
      final panel = tester.getRect(find.byType(LeftPanel));
      expect(tester.getRect(find.text(long)).right, lessThan(panel.right));
    }

    testWidgets("a long bus name stays inside a take's OUTPUT row", (
      tester,
    ) async {
      await pump(
        tester,
        track: _vocal(busId: 'a'),
        buses: [_bus('a', long)],
      );
      expectInsidePanel(tester);
    });

    testWidgets("a long bus name stays inside a bus's OUTPUT row", (
      tester,
    ) async {
      final routed = FxBus(
        id: 'b',
        name: 'Delay',
        fx: TrackFx.flat(),
        outputBusId: 'a',
      );
      await pump(tester, bus: routed, buses: [_bus('a', long), routed]);
      expectInsidePanel(tester);
    });

    testWidgets("a bus's own output never offers itself", (tester) async {
      await pump(
        tester,
        bus: _bus('a', 'Reverb'),
        buses: [_bus('a', 'Reverb'), _bus('b', 'Delay')],
      );

      await tester.tap(find.text('Stereo Out'));
      await tester.pumpAndSettle();
      // Only the other bus is a legal destination; routing a bus into itself
      // is a feedback loop the menu must not even offer.
      expect(find.text('Delay'), findsOneWidget);
      expect(
        find.text('Reverb'),
        findsOneWidget,
        reason: "the bus's own header only — never itself as a destination",
      );
    });
  });

  // Regression guard: the inspector built its FxPanel without a key, so
  // selecting another take (or bus) reused the panel's element tree and the
  // EQ's live state — the analyzer's smoothed spectrum, adaptive ceiling and
  // latched "has signal" flag — carried over from the previous channel.
  group('selecting another channel gives the FX panel fresh state', () {
    final eqOn = TrackFx.flat().copyWith(eqOn: true);
    State<StatefulWidget> spectrum(WidgetTester tester) => tester.state(
      find.byWidgetPredicate(
        (w) => w.runtimeType.toString() == '_EqSpectrumLayer',
      ),
    );

    testWidgets('take to take', (tester) async {
      await pump(tester, track: _vocal(fx: eqOn));
      final before = spectrum(tester);
      await pump(
        tester,
        track: _vocal(id: 't2', fx: eqOn),
      );
      expect(spectrum(tester), isNot(same(before)));
      expect(tester.takeException(), isNull);
    });

    testWidgets('bus to bus', (tester) async {
      final a = FxBus(id: 'a', name: 'Reverb', fx: eqOn);
      final b = FxBus(id: 'b', name: 'Delay', fx: eqOn);
      await pump(tester, bus: a, buses: [a, b]);
      final before = spectrum(tester);
      await pump(tester, bus: b, buses: [a, b]);
      expect(spectrum(tester), isNot(same(before)));
      expect(tester.takeException(), isNull);
    });
  });
}
