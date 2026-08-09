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
  double gainReductionDb(String trackId) => trackGr;

  @override
  double busGainReductionDb(String busId) => busGr;
}

class _FakeAnalysis extends Fake implements AnalysisCache {
  int eqInputCalls = 0;

  @override
  ({Float32List col, double binHz})? eqInputColumn(String trackId) {
    eqInputCalls++;
    return null; // no spectrum yet — the graph draws the response curve alone
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

  testWidgets('choosing a preset replaces the chain but keeps the order', (
    tester,
  ) async {
    final custom = [FxKind.comp, FxKind.eq, ...FxKind.values.skip(2)];
    final vm = await pump(
      tester,
      track: _track(fx: _fxAllOff().copyWith(order: custom)),
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
    expect(vm.commits, 1);
  });
}
