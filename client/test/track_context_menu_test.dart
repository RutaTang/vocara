import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/ui/song_practice_detail/widgets/track_context_menu.dart';

TrackMenuActions _actions({
  bool isVocal = true,
  bool canSeparate = false,
  bool canExport = false,
  VoidCallback? onAddClip,
}) {
  return TrackMenuActions(
    isVocal: isVocal,
    canMergeDown: false,
    canSeparate: canSeparate,
    canExport: canExport,
    reseparate: false,
    currentColorValue: null,
    onPickColor: (_) {},
    onRename: () {},
    onSeparate: () {},
    onExport: () {},
    onAddNote: () {},
    onAddTechnique: () {},
    onAddClip: onAddClip ?? () {},
    onDuplicate: () {},
    onSplit: () {},
    onMergeDown: () {},
    onReanalyze: () {},
    onDelete: () {},
  );
}

/// Pumps a host screen and opens the track context menu with [actions].
Future<void> _openTrackMenu(
  WidgetTester tester,
  TrackMenuActions actions,
) async {
  await tester.pumpWidget(
    MaterialApp(
      home: Scaffold(
        body: Builder(
          builder: (context) => TextButton(
            onPressed: () =>
                showTrackContextMenu(context, const Offset(40, 40), actions),
            child: const Text('open'),
          ),
        ),
      ),
    ),
  );
  await tester.tap(find.text('open'));
  await tester.pump();
}

void main() {
  group('track context menu separation gating', () {
    testWidgets('hides "Extract backing" when the engine is unavailable', (
      tester,
    ) async {
      // The screen passes canSeparate: track.isReference && vm.separationAvailable,
      // so an unavailable engine reaches the menu as canSeparate: false.
      await _openTrackMenu(tester, _actions(isVocal: false));
      expect(find.text('Extract backing'), findsNothing);
      expect(find.text('Re-extract backing'), findsNothing);
    });

    testWidgets('shows "Extract backing" when the engine is available', (
      tester,
    ) async {
      await _openTrackMenu(tester, _actions(isVocal: false, canSeparate: true));
      expect(find.text('Extract backing'), findsOneWidget);
    });
  });

  group('track context menu export', () {
    testWidgets('offers "Export take…" for a vocal lane with audio', (
      tester,
    ) async {
      await _openTrackMenu(tester, _actions(canExport: true));
      expect(find.text('Export take…'), findsOneWidget);
    });

    testWidgets('offers "Export audio…" for a backing lane with audio', (
      tester,
    ) async {
      await _openTrackMenu(tester, _actions(isVocal: false, canExport: true));
      expect(find.text('Export audio…'), findsOneWidget);
    });

    testWidgets('hides export for a lane with no audio', (tester) async {
      await _openTrackMenu(tester, _actions());
      expect(find.text('Export take…'), findsNothing);
      expect(find.text('Export audio…'), findsNothing);
    });
  });

  group('track context menu add clip', () {
    testWidgets('offers "Add audio clip at playhead…" on a vocal lane', (
      tester,
    ) async {
      // The click-to-upload twin of dropping audio onto the lane.
      var added = 0;
      await _openTrackMenu(tester, _actions(onAddClip: () => added++));
      final item = find.text('Add audio clip at playhead…');
      expect(item, findsOneWidget);
      await tester.tap(item);
      await tester.pump();
      expect(added, 1);
    });

    testWidgets('hides it on a backing lane (clips are a takes feature)', (
      tester,
    ) async {
      await _openTrackMenu(tester, _actions(isVocal: false));
      expect(find.text('Add audio clip at playhead…'), findsNothing);
    });
  });

  group('clip context menu export', () {
    Future<void> openClipMenu(
      WidgetTester tester, {
      VoidCallback? onExport,
    }) async {
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: Builder(
              builder: (context) => TextButton(
                onPressed: () => showClipContextMenu(
                  context,
                  const Offset(40, 40),
                  onDelete: () {},
                  onExport: onExport,
                ),
                child: const Text('open'),
              ),
            ),
          ),
        ),
      );
      await tester.tap(find.text('open'));
      await tester.pump();
    }

    testWidgets('offers "Export clip…" and runs the action', (tester) async {
      var exported = false;
      await openClipMenu(tester, onExport: () => exported = true);
      expect(find.text('Export clip…'), findsOneWidget);
      await tester.tap(find.text('Export clip…'));
      await tester.pump();
      expect(exported, isTrue);
    });

    testWidgets('omits the row when no export action is wired', (tester) async {
      await openClipMenu(tester);
      expect(find.text('Export clip…'), findsNothing);
      expect(find.text('Delete clip'), findsOneWidget);
    });
  });
}
