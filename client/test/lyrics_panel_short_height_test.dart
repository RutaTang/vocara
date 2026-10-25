// The right panel beside a short timeline. The dock can be dragged until the
// timeline (and the side panels beside it) keep only their 120 px floor, and
// on the minimum 960x600 window the dock's default height already leaves the
// panels 173 px. The lyrics editor's Save/Cancel and the sync view's
// Clear/Tap/Done sat below a fixed toolbar and hint there, pushed under the
// dock and out of reach.
//
// Laid out in Roboto from the Flutter SDK rather than the em-wide test font,
// whose glyph widths overflow rows that real fonts fit.

import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/lyric_line.dart';
import 'package:vocara/domain/models/practice_note.dart';
import 'package:vocara/ui/core/widgets/studio_button.dart';
import 'package:vocara/ui/song_practice_detail/view_model/song_practice_detail_view_model.dart';
import 'package:vocara/ui/song_practice_detail/widgets/lyrics_panel.dart';

/// Just what the lyrics tab reads in Edit and Sync mode.
class _FakeVm extends Fake implements SongPracticeDetailViewModel {
  _FakeVm(this.mode);

  LyricsMode mode;
  String? applied;
  int stamps = 0;

  @override
  bool get lyricsOpen => true;
  @override
  RightTab get rightTab => RightTab.lyrics;
  @override
  void toggleLyricsPanel() {}
  @override
  AudioTrack? get selectedTrack => null;
  @override
  List<({String trackId, String trackName, PracticeNote note})> get allNotes =>
      const [];

  @override
  bool get hasLyrics => true;
  @override
  List<LyricLine> get lyrics => [
    for (var i = 0; i < 12; i++) LyricLine(text: 'line $i'),
  ];
  @override
  final ValueNotifier<Duration> position = ValueNotifier(Duration.zero);
  @override
  int currentLyricIndex(int positionMs) => -1;

  @override
  LyricsMode get lyricsMode => mode;
  @override
  void setLyricsMode(LyricsMode m) => mode = m;
  @override
  void setLyricsFocus(bool on) {}
  @override
  String? lyricsDraft;
  @override
  int lyricsEditSession = 1;
  @override
  String lyricsAsText() => 'line 0\nline 1';
  @override
  Future<bool> applyEditedLyrics(String text) async {
    applied = text;
    return true;
  }

  @override
  int get syncIndex => 0;
  @override
  Future<void> stampNextLyric() async => stamps++;
  @override
  Future<void> stampLyricAt(int index) async {}
  @override
  Future<void> clearLyricTimes() async {}
}

/// Roboto ships with every Flutter SDK (the material fonts artifact).
Future<void> _loadRoboto() async {
  final root =
      Platform.environment['FLUTTER_ROOT'] ??
      File(Platform.resolvedExecutable).parent.parent.parent.parent.parent.path;
  final dir = '$root/bin/cache/artifacts/material_fonts';
  final loader = FontLoader('Roboto');
  for (final f in [
    'Roboto-Regular.ttf',
    'Roboto-Medium.ttf',
    'Roboto-Bold.ttf',
  ]) {
    final file = File('$dir/$f');
    expect(file.existsSync(), isTrue, reason: 'Roboto not found in $dir');
    loader.addFont(Future.value(ByteData.sublistView(file.readAsBytesSync())));
  }
  await loader.load();
}

/// The panel as the workspace lays it out: as tall as the dock leaves it.
Future<void> _pump(WidgetTester tester, _FakeVm vm, double height) async {
  tester.view.physicalSize = const Size(1440, 900);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(
    MaterialApp(
      theme: ThemeData(fontFamily: 'Roboto'),
      home: Scaffold(
        body: Align(
          alignment: Alignment.topLeft,
          child: SizedBox(
            height: height,
            child: RightPanel(
              vm: vm,
              onAddNote: (_) {},
              onEditNote: (_, _) {},
              onAddTechnique: (_) {},
              onEditTechnique: (_, _) {},
            ),
          ),
        ),
      ),
    ),
  );
  await tester.pump();
}

const _syncHint =
    'Play, then press Space (or Tap) on each line as you reach it. '
    'Click a line to re-time it.';

void main() {
  setUpAll(_loadRoboto);

  const actions = {
    LyricsMode.edit: ['Cancel', 'Save'],
    LyricsMode.sync: ['Clear', 'Tap (Space)', 'Done'],
  };

  for (final MapEntry(key: mode, value: labels) in actions.entries) {
    // 173: the minimum window with the dock open at its default height.
    // 120: the timeline's floor, the dock dragged as tall as it goes.
    for (final height in [173.0, 120.0]) {
      testWidgets('$mode keeps ${labels.join(' / ')} on screen at a '
          '$height px panel', (tester) async {
        await _pump(tester, _FakeVm(mode), height);

        expect(tester.takeException(), isNull, reason: 'no overflow');
        final panel = tester.getRect(find.byType(RightPanel));
        for (final label in labels) {
          final button = tester.getRect(find.text(label));
          expect(
            panel.contains(button.topLeft) &&
                panel.contains(button.bottomRight - const Offset(1, 1)),
            isTrue,
            reason: '$label at $button lies outside the panel $panel',
          );
        }
      });
    }
  }

  testWidgets('at the floor the actions still take their taps', (tester) async {
    final sync = _FakeVm(LyricsMode.sync);
    await _pump(tester, sync, 120);
    await tester.tap(find.text('Tap (Space)'));
    expect(sync.stamps, 1);
    await tester.tap(find.text('Done'));
    expect(sync.mode, LyricsMode.read);

    final edit = _FakeVm(LyricsMode.edit);
    await _pump(tester, edit, 120);
    await tester.tap(find.text('Save'));
    await tester.pump();
    expect(edit.applied, isNotNull);
  });

  testWidgets('a tall panel keeps its toolbar and the pinned sync hint', (
    tester,
  ) async {
    await _pump(tester, _FakeVm(LyricsMode.sync), 600);
    expect(tester.takeException(), isNull);
    expect(find.byTooltip('Sync (tap-to-time)'), findsOneWidget);
    expect(
      find.ancestor(
        of: find.text(_syncHint),
        matching: find.byType(Scrollable),
      ),
      findsNothing,
      reason: 'the hint stays pinned above the lines',
    );

    await _pump(tester, _FakeVm(LyricsMode.edit), 600);
    expect(find.byTooltip('Edit lyrics'), findsOneWidget);
    final panel = tester.getRect(find.byType(RightPanel));
    final save = find.ancestor(
      of: find.text('Save'),
      matching: find.byType(StudioButton),
    );
    expect(
      panel.bottom - tester.getRect(save).bottom,
      12,
      reason: 'the editor keeps its normal spacing',
    );
  });

  testWidgets('the sync hint scrolls with the lines on a short panel', (
    tester,
  ) async {
    await _pump(tester, _FakeVm(LyricsMode.sync), 173);
    expect(
      find.ancestor(
        of: find.text(_syncHint),
        matching: find.byType(Scrollable),
      ),
      findsOneWidget,
    );
  });
}
