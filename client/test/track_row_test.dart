import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/ui/song_practice_detail/widgets/track_row.dart';

void main() {
  testWidgets('M/S toggles fire on a reorderable lane (wrapped in Draggable)',
      (tester) async {
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

    testWidgets('dragging a clip beyond the viewport still commits on release',
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
    });

    testWidgets('a mid-drag cull cannot transplant the drag onto the next clip',
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
      await gesture.moveBy(const Offset(420, 0)); // clip a leaves the viewport
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
    });

    testWidgets('pointer cancel mid-drag still commits the live moves',
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

    testWidgets('a plain tap selects the clip and never commits a move',
        (tester) async {
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
  });
}
