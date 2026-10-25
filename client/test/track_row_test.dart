import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/gestures.dart' show PointerDeviceKind;
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/ui/core/widgets/waveform_painter.dart';
import 'package:vocara/ui/song_practice_detail/widgets/track_row.dart';

/// Rasterizes [painter] at [size] and returns the x-center of the topmost
/// painted row — with a single full-amplitude spike among zero peaks, that is
/// the spike's drawn timeline position (centering cancels the antialiasing
/// skirt on the flanking slopes).
Future<double> _topmostPaintedX(
  WidgetTester tester,
  CustomPainter painter,
  Size size,
) async {
  final rec = ui.PictureRecorder();
  painter.paint(Canvas(rec), size);
  final picture = rec.endRecording();
  double? found;
  await tester.runAsync(() async {
    final img = await picture.toImage(size.width.ceil(), size.height.ceil());
    final data = (await img.toByteData())!;
    for (var y = 0; y < img.height && found == null; y++) {
      int? lo, hi;
      for (var x = 0; x < img.width; x++) {
        if (data.getUint8((y * img.width + x) * 4 + 3) > 128) {
          lo ??= x;
          hi = x;
        }
      }
      if (lo != null) found = (lo + hi!) / 2;
    }
    img.dispose();
  });
  expect(found, isNotNull, reason: 'nothing painted');
  return found!;
}

void main() {
  testWidgets('M/S toggles fire on a reorderable lane (wrapped in Draggable)', (
    tester,
  ) async {
    var mute = 0, solo = 0, select = 0;
    final track = AudioTrack(
      id: 't1',
      kind: AudioTrackKind.vocal,
      displayName: 'Take 1',
      audioFilePath: '/x.wav',
      durationMs: 5000,
      createdAt: DateTime(2020),
    );
    final pos = ValueNotifier<Duration>(Duration.zero);
    addTearDown(pos.dispose);

    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: TrackRow(
            track: track,
            label: 'Take 1',
            timelineDurationMs: 10000,
            position: pos,
            selected: false,
            reorderable: true,
            onSelect: () => select++,
            onClipMoveLive: (_, _) {},
            onMoveCommit: () {},
            onSelectClip: (_) {},
            onClipContextMenu: (_, _) {},
            onCrossLaneMove: (_) {},
            onContextMenu: (_) {},
            onAddNote: () {},
            onEditNote: (_) {},
            onAddTechnique: () {},
            onToggleMute: () => mute++,
            onToggleSolo: () => solo++,
            onVolume: (_) {},
            onVolumeCommit: () {},
            onDelete: () {},
          ),
        ),
      ),
    );

    await tester.tap(find.text('M'));
    await tester.pump();
    await tester.tap(find.text('S'));
    await tester.pump();

    expect(mute, 1, reason: 'tapping M should toggle mute');
    expect(solo, 1, reason: 'tapping S should toggle solo');
    expect(select, 0, reason: 'tapping M/S should not select the lane');
  });

  // --- Clip move-drag regression guards -------------------------------------
  //
  // The clip lane culls clips that scroll outside the zoomed viewport. An
  // in-flight move-drag used to be torn down by that cull: the region
  // unmounted mid-gesture, so onHorizontalDragEnd (the persistence + engine
  // commit) never fired — and because the Positioned children were keyless,
  // the orphaned drag state could transplant onto the NEXT clip by index and
  // keep moving the wrong clip. These tests drive a real drag past the
  // viewport edge, rebuilding on every live tick like the VM does.

  group('clip move drag', () {
    // Lane geometry: screen 800 wide, header kTrackHeaderW, viewport 10s.
    const viewportMs = 10000;
    const laneW = 800.0 - kTrackHeaderW;

    Offset clipCenter(int startMs, {int durMs = 1000}) {
      final left = kTrackHeaderW + startMs / viewportMs * laneW;
      final width = durMs / viewportMs * laneW;
      return Offset(left + width / 2, kTrackRowHeight / 2);
    }

    /// Pumps a lane whose clip moves are applied back into the widget on every
    /// live tick (like the real VM's notifyListeners), so the viewport cull
    /// re-evaluates mid-drag exactly as it does in the app.
    Future<void> pumpClipLane(
      WidgetTester tester, {
      required Map<String, int> starts,
      required List<String> liveIds,
      required VoidCallback onCommit,
      ValueChanged<String>? onSelectClip,
    }) async {
      final pos = ValueNotifier<Duration>(Duration.zero);
      addTearDown(pos.dispose);
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: StatefulBuilder(
              builder: (context, setState) => TrackRow(
                track: AudioTrack(
                  id: 't1',
                  kind: AudioTrackKind.vocal,
                  displayName: 'Take 1',
                  createdAt: DateTime(2026),
                  clips: [
                    for (final e in starts.entries)
                      AudioClip(
                        id: e.key,
                        audioFilePath: '/${e.key}.wav',
                        durationMs: 1000,
                        startMs: e.value,
                        waveformPeaks: const [0.4, 0.8, 0.4],
                      ),
                  ],
                ),
                label: 'Take 1',
                timelineDurationMs: viewportMs,
                position: pos,
                selected: false,
                reorderable: false,
                onSelect: () {},
                onClipMoveLive: (id, ms) => setState(() {
                  liveIds.add(id);
                  // Mimic the VM's clamp against the FULL timeline (60s here),
                  // which is far wider than the zoomed 10s viewport — so a
                  // clip CAN move fully outside the visible window.
                  starts[id] = ms < 0 ? 0 : (ms > 59000 ? 59000 : ms);
                }),
                onMoveCommit: onCommit,
                onSelectClip: onSelectClip ?? (_) {},
                onClipContextMenu: (_, _) {},
                onCrossLaneMove: (_) {},
                onContextMenu: (_) {},
                onAddNote: () {},
                onEditNote: (_) {},
                onAddTechnique: () {},
                onToggleMute: () {},
                onToggleSolo: () {},
                onVolume: (_) {},
                onVolumeCommit: () {},
                onDelete: () {},
              ),
            ),
          ),
        ),
      );
    }

    testWidgets(
      'dragging a clip beyond the viewport still commits on release',
      (tester) async {
        final starts = {'a': 4000};
        final liveIds = <String>[];
        var commits = 0;
        await pumpClipLane(
          tester,
          starts: starts,
          liveIds: liveIds,
          onCommit: () => commits++,
        );

        final gesture = await tester.startGesture(clipCenter(4000));
        await gesture.moveBy(const Offset(30, 0)); // cross slop → drag accepted
        await tester.pump();
        await gesture.moveBy(const Offset(420, 0)); // push past the right edge
        await tester.pump();
        expect(
          starts['a']!,
          greaterThan(viewportMs),
          reason: 'the clip should now be fully outside the zoomed viewport',
        );
        await gesture.moveBy(const Offset(12, 0)); // drag must still be alive
        await tester.pump();
        await gesture.up();
        await tester.pump();

        expect(tester.takeException(), isNull);
        expect(liveIds, isNotEmpty);
        expect(
          commits,
          1,
          reason: 'releasing an off-viewport drag must still commit the move',
        );
      },
    );

    testWidgets(
      'a mid-drag cull cannot transplant the drag onto the next clip',
      (tester) async {
        final starts = {'a': 4000, 'b': 8000};
        final liveIds = <String>[];
        var commits = 0;
        await pumpClipLane(
          tester,
          starts: starts,
          liveIds: liveIds,
          onCommit: () => commits++,
        );

        final gesture = await tester.startGesture(clipCenter(4000));
        await gesture.moveBy(const Offset(30, 0));
        await tester.pump();
        await gesture.moveBy(
          const Offset(420, 0),
        ); // clip a leaves the viewport
        await tester.pump();
        await gesture.moveBy(const Offset(12, 0));
        await tester.pump();
        await gesture.up();
        await tester.pump();

        expect(tester.takeException(), isNull);
        expect(
          liveIds.toSet(),
          {'a'},
          reason: 'every live tick must keep targeting the dragged clip',
        );
        expect(starts['b'], 8000, reason: 'the neighbor clip must not move');
        expect(commits, 1);
      },
    );

    testWidgets('pointer cancel mid-drag still commits the live moves', (
      tester,
    ) async {
      final starts = {'a': 4000};
      final liveIds = <String>[];
      var commits = 0;
      await pumpClipLane(
        tester,
        starts: starts,
        liveIds: liveIds,
        onCommit: () => commits++,
      );

      final gesture = await tester.startGesture(clipCenter(4000));
      await gesture.moveBy(const Offset(30, 0));
      await tester.pump();
      await gesture.moveBy(const Offset(50, 0)); // a real live move lands
      await tester.pump();
      await gesture.cancel();
      await tester.pump();

      expect(tester.takeException(), isNull);
      expect(liveIds, isNotEmpty);
      expect(
        commits,
        1,
        reason: 'a cancelled drag already moved the model — it must commit',
      );
    });

    testWidgets('a plain tap selects the clip and never commits a move', (
      tester,
    ) async {
      final starts = {'a': 4000};
      var commits = 0;
      String? selected;
      await pumpClipLane(
        tester,
        starts: starts,
        liveIds: <String>[],
        onCommit: () => commits++,
        onSelectClip: (id) => selected = id,
      );

      // A tap makes the drag recognizer lose the arena, which fires
      // onHorizontalDragCancel — that teardown must NOT commit anything.
      await tester.tapAt(clipCenter(4000));
      await tester.pump();

      expect(selected, 'a');
      expect(starts['a'], 4000);
      expect(commits, 0);
    });

    // A two-finger trackpad swipe is how the timeline pans, and it reaches
    // the clip under the cursor as a pan-zoom gesture — which a drag
    // recognizer accepts unless told otherwise. It moved, and saved, the clip
    // the cursor happened to rest on.
    testWidgets('a two-finger trackpad swipe over a clip never moves it', (
      tester,
    ) async {
      final starts = {'a': 4000};
      final liveIds = <String>[];
      var commits = 0;
      await pumpClipLane(
        tester,
        starts: starts,
        liveIds: liveIds,
        onCommit: () => commits++,
      );

      final at = clipCenter(4000);
      final g = await tester.createGesture(kind: PointerDeviceKind.trackpad);
      await g.panZoomStart(at);
      await tester.pump();
      await g.panZoomUpdate(at, pan: const Offset(-60, 0));
      await tester.pump();
      await g.panZoomUpdate(at, pan: const Offset(-200, 0));
      await tester.pump();
      await g.panZoomEnd();
      await tester.pump();

      expect(liveIds, isEmpty, reason: 'a swipe pans the view, not the clip');
      expect(starts['a'], 4000);
      expect(commits, 0);
    });

    testWidgets('a mouse drag still moves and commits the clip', (
      tester,
    ) async {
      final starts = {'a': 4000};
      final liveIds = <String>[];
      var commits = 0;
      await pumpClipLane(
        tester,
        starts: starts,
        liveIds: liveIds,
        onCommit: () => commits++,
      );

      final g = await tester.startGesture(
        clipCenter(4000),
        kind: PointerDeviceKind.mouse,
      );
      await g.moveBy(const Offset(30, 0));
      await tester.pump();
      await g.moveBy(const Offset(50, 0));
      await tester.pump();
      await g.up();
      await tester.pump();

      expect(liveIds, isNotEmpty);
      expect(starts['a'], greaterThan(4000));
      expect(commits, 1);
    });
  });

  // --- Viewport waveform alignment ------------------------------------------
  //
  // The lane culls a clip's peaks to the viewport by slicing at WHOLE peak
  // indices, but the drawn box starts at the un-quantized viewport edge. The
  // painter must carry the discarded fraction, or the whole waveform draws up
  // to one peak interval late against the ruler/grid/playhead — and while
  // panning it freezes on screen, then snaps a bucket at a time.

  group('viewport waveform alignment', () {
    Widget lane({
      required AudioTrack track,
      required int viewSpanMs,
      required int viewStartMs,
      ValueListenable<List<double>>? recordingPeaks,
    }) {
      final pos = ValueNotifier<Duration>(Duration.zero);
      addTearDown(pos.dispose);
      return MaterialApp(
        home: Scaffold(
          body: TrackRow(
            track: track,
            label: 'Take 1',
            timelineDurationMs: viewSpanMs,
            viewStartMs: viewStartMs,
            position: pos,
            selected: false,
            reorderable: false,
            recordingPeaks: recordingPeaks,
            onSelect: () {},
            onClipMoveLive: (_, _) {},
            onMoveCommit: () {},
            onSelectClip: (_) {},
            onClipContextMenu: (_, _) {},
            onCrossLaneMove: (_) {},
            onContextMenu: (_) {},
            onAddNote: () {},
            onEditNote: (_) {},
            onAddTechnique: () {},
            onToggleMute: () {},
            onToggleSolo: () {},
            onVolume: (_) {},
            onVolumeCommit: () {},
            onDelete: () {},
          ),
        ),
      );
    }

    testWidgets(
      'a clip sliced at the viewport edge draws its transient at the true '
      'timeline position',
      (tester) async {
        // Clip 0..10 s with 10 peaks (1 s per peak); the spike is peak 5,
        // anchored at 5000 ms. Viewport 4500..9500: the slice starts at peak
        // 4 (4000 ms), HALF a peak before the box's left edge.
        final track = AudioTrack(
          id: 't1',
          kind: AudioTrackKind.vocal,
          displayName: 'Take 1',
          createdAt: DateTime(2026),
          audioFilePath: '/x.wav',
          durationMs: 10000,
          waveformPeaks: const [0, 0, 0, 0, 0, 1, 0, 0, 0, 0],
        );
        await tester.pumpWidget(
          lane(track: track, viewSpanMs: 5000, viewStartMs: 4500),
        );

        final painter = tester
            .widgetList<CustomPaint>(find.byType(CustomPaint))
            .map((c) => c.painter)
            .whereType<WaveformPainter>()
            .single;
        // Paint standalone at a round width: the spike's true position is
        // (5000 − 4500) / 5000 of the box. Anchoring the slice at the box
        // edge instead would draw it at 1/5 of the width (100), one half-peak
        // (500 ms) late against the ruler.
        const size = Size(500, 40);
        final apexX = await _topmostPaintedX(tester, painter, size);
        expect(
          apexX,
          closeTo(50, 2),
          reason: 'the transient must line up with its ruler time',
        );
      },
    );

    testWidgets(
      'the live forming clip keeps its absolute sample-to-column mapping when '
      'the viewport edge lands mid-sample',
      (tester) async {
        // 100 record peaks of 50 ms (take 0..5000 ms), spike at peak 30
        // (1500 ms). Viewport 1025..2025: the slice starts at peak 20
        // (1000 ms), half a record-peak before the box edge.
        final peaks = ValueNotifier<List<double>>([
          for (var i = 0; i < 100; i++) i == 30 ? 1.0 : 0.0,
        ]);
        addTearDown(peaks.dispose);
        final track = AudioTrack(
          id: 't1',
          kind: AudioTrackKind.vocal,
          displayName: 'Take 1',
          createdAt: DateTime(2026),
        );
        await tester.pumpWidget(
          lane(
            track: track,
            viewSpanMs: 1000,
            viewStartMs: 1025,
            recordingPeaks: peaks,
          ),
        );

        final painter = tester
            .widgetList<CustomPaint>(find.byType(CustomPaint))
            .map((c) => c.painter)
            .whereType<LiveWaveformPainter>()
            .single;
        // The spike bucket covers 1500..1550 ms; its drawn center belongs
        // (1525 − 1025) / 50 record-peaks right of the box edge, at the
        // painter's own absolute pixels-per-sample. Re-basing the slice to
        // the box edge would draw it half a peak (~15 px here) late, and it
        // would freeze on screen while panning.
        final expected = (1525 - 1025) / 50 * painter.pixelsPerSample;
        const size = Size(500, 40);
        final centerX = await _topmostPaintedX(tester, painter, size);
        expect(
          centerX,
          closeTo(expected, 3),
          reason: 'the live waveform must stay glued to the timeline',
        );
      },
    );
  });
}
