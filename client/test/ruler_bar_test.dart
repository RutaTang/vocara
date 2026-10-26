// The ruler's loop-region handles. A two-finger trackpad swipe is how the
// timeline pans, and with the cursor resting on a handle it reached the
// handle's drag as a pan-zoom gesture — moving the loop edge (and snapping
// the playhead into the new region) instead of panning. The ruler body has
// excluded trackpads from its scrub for the same reason.

import 'package:flutter/gestures.dart' show PointerDeviceKind;
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/ui/song_practice_detail/view_model/playback_controller.dart';
import 'package:vocara/ui/song_practice_detail/view_model/song_practice_detail_view_model.dart';
import 'package:vocara/ui/song_practice_detail/widgets/ruler_bar.dart';
import 'package:vocara/ui/song_practice_detail/widgets/track_row.dart';

class _FakePlayback extends Fake implements PlaybackController {
  _FakePlayback(this.viewStartMs, this.loopStartMs, this.loopEndMs);

  final starts = <int>[];
  final ends = <int>[];

  @override
  final int viewStartMs;

  @override
  final int loopStartMs;

  @override
  final int loopEndMs;

  @override
  bool get loopEnabled => true;

  @override
  void setLoopStart(int ms) => starts.add(ms);

  @override
  void setLoopEnd(int ms) => ends.add(ms);
}

class _FakeVm extends Fake implements SongPracticeDetailViewModel {
  _FakeVm(int viewStartMs, int loopStartMs, int loopEndMs)
    : fakePlayback = _FakePlayback(viewStartMs, loopStartMs, loopEndMs);

  final _FakePlayback fakePlayback;
  final seeks = <int>[];

  @override
  PlaybackController get playback => fakePlayback;

  @override
  Future<void> seek(Duration to) async => seeks.add(to.inMilliseconds);
}

void main() {
  const timelineMs = 10000;
  const screenW = 800.0;
  const laneW = screenW - 2 * kTimelineHMargin - kTrackHeaderW;

  Future<_FakeVm> pumpRuler(
    WidgetTester tester, {
    int viewStartMs = 0,
    int viewMs = timelineMs,
    int loopStartMs = 2000,
    int loopEndMs = 8000,
  }) async {
    final vm = _FakeVm(viewStartMs, loopStartMs, loopEndMs);
    final position = ValueNotifier(Duration.zero);
    addTearDown(position.dispose);
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: Align(
            alignment: Alignment.topLeft,
            child: SizedBox(
              width: screenW,
              child: RulerBar(
                vm: vm,
                timelineDurationMs: viewMs,
                position: position,
              ),
            ),
          ),
        ),
      ),
    );
    return vm;
  }

  /// The centre of the loop-end handle (8000 ms of the 10 s view).
  const endHandle = Offset(
    kTimelineHMargin + kTrackHeaderW + 8000 / timelineMs * laneW,
    rulerHeight / 2,
  );

  /// The ruler's x for [ms] of the 10 s view.
  Offset at(double ms) => Offset(
    kTimelineHMargin + kTrackHeaderW + ms / timelineMs * laneW,
    rulerHeight / 2,
  );

  testWidgets('a click on a loop handle still seeks', (tester) async {
    // The default loop starts at 0: its handle covered the start of the
    // ruler, and a click there to go back to 0:00 did nothing.
    final vm = await pumpRuler(tester, loopStartMs: 0);
    await tester.tapAt(at(0) + const Offset(3, 0));
    await tester.pump();
    expect(vm.seeks, hasLength(1));
    expect(vm.seeks.single, closeTo(3 / laneW * timelineMs, 2));
    expect(vm.fakePlayback.starts, isEmpty);
  });

  testWidgets('a loop narrower than a handle still moves left', (tester) async {
    // The end's handle lies over the start's: a leftward drag took the end,
    // which stops at the start, so the region could not move left at all.
    final vm = await pumpRuler(tester, loopStartMs: 5000, loopEndMs: 5100);
    final g = await tester.startGesture(
      at(5050),
      kind: PointerDeviceKind.mouse,
    );
    await g.moveBy(const Offset(-10, 0)); // past the drag slop
    await tester.pump();
    await g.moveBy(const Offset(-20, 0));
    await tester.pump();
    await g.up();
    await tester.pump();
    expect(vm.fakePlayback.starts, isNotEmpty);
    expect(vm.fakePlayback.starts.last, lessThan(5000));
    expect(vm.fakePlayback.ends, isEmpty);

    final right = await tester.startGesture(
      at(5050),
      kind: PointerDeviceKind.mouse,
    );
    await right.moveBy(const Offset(10, 0));
    await tester.pump();
    await right.moveBy(const Offset(20, 0));
    await tester.pump();
    await right.up();
    await tester.pump();
    expect(vm.fakePlayback.ends.last, greaterThan(5100));
  });

  testWidgets('a two-finger trackpad swipe over a loop handle leaves it', (
    tester,
  ) async {
    final vm = await pumpRuler(tester);
    final g = await tester.createGesture(kind: PointerDeviceKind.trackpad);
    await g.panZoomStart(endHandle);
    await tester.pump();
    await g.panZoomUpdate(endHandle, pan: const Offset(-40, 0));
    await tester.pump();
    await g.panZoomUpdate(endHandle, pan: const Offset(-120, 0));
    await tester.pump();
    await g.panZoomEnd();
    await tester.pump();
    expect(vm.fakePlayback.ends, isEmpty);
    expect(vm.fakePlayback.starts, isEmpty);
  });

  testWidgets('a mouse drag still moves the loop edge', (tester) async {
    final vm = await pumpRuler(tester);
    final g = await tester.startGesture(
      endHandle,
      kind: PointerDeviceKind.mouse,
    );
    await g.moveBy(const Offset(-30, 0));
    await tester.pump();
    await g.moveBy(const Offset(-30, 0));
    await tester.pump();
    await g.up();
    await tester.pump();
    expect(vm.fakePlayback.ends, isNotEmpty);
    expect(vm.fakePlayback.ends.last, lessThan(8000));
  });

  testWidgets('a slow drag at deep zoom moves the edge as far as the pointer', (
    tester,
  ) async {
    // Each tick added its own rounded delta: at about 1.3 ms per px a quarter
    // pixel rounds to nothing, so the handle never moved.
    const viewMs = 1000;
    final vm = await pumpRuler(tester, viewStartMs: 7500, viewMs: viewMs);
    final g = await tester.startGesture(
      const Offset(
        kTimelineHMargin + kTrackHeaderW + 500 / viewMs * laneW,
        rulerHeight / 2,
      ),
      kind: PointerDeviceKind.mouse,
    );
    await g.moveBy(const Offset(4, 0)); // past the drag slop
    await tester.pump();
    final from = vm.fakePlayback.ends.lastOrNull ?? 8000;
    for (var i = 0; i < 240; i++) {
      await g.moveBy(const Offset(0.25, 0));
    }
    await g.up();
    await tester.pump();
    final moved = vm.fakePlayback.ends.last - from;
    expect(moved, closeTo(60 / laneW * viewMs, 2));
  });
}
