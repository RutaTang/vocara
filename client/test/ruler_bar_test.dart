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
  final starts = <int>[];
  final ends = <int>[];

  @override
  int get viewStartMs => 0;

  @override
  int get loopStartMs => 2000;

  @override
  int get loopEndMs => 8000;

  @override
  bool get loopEnabled => true;

  @override
  void setLoopStart(int ms) => starts.add(ms);

  @override
  void setLoopEnd(int ms) => ends.add(ms);
}

class _FakeVm extends Fake implements SongPracticeDetailViewModel {
  final fakePlayback = _FakePlayback();

  @override
  PlaybackController get playback => fakePlayback;

  @override
  Future<void> seek(Duration to) async {}
}

void main() {
  const timelineMs = 10000;
  const screenW = 800.0;
  const laneW = screenW - 2 * kTimelineHMargin - kTrackHeaderW;

  Future<_FakeVm> pumpRuler(WidgetTester tester) async {
    final vm = _FakeVm();
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
                timelineDurationMs: timelineMs,
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
}
