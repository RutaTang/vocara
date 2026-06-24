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
}
