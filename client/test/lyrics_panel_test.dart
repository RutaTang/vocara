import 'dart:async';

import 'package:desktop_drop/desktop_drop.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/lyric_line.dart';
import 'package:vocara/domain/models/practice_note.dart';
import 'package:vocara/ui/song_practice_detail/view_model/song_practice_detail_view_model.dart';
import 'package:vocara/ui/song_practice_detail/widgets/lyrics_panel.dart';

/// Minimal fake: only the members RightPanel touches with the lyrics tab open
/// in EDIT mode, plus the import/apply hooks the drop flow drives.
class _FakeVm extends Fake implements SongPracticeDetailViewModel {
  String text = 'old line';
  LyricsMode mode = LyricsMode.edit;
  RightTab tab = RightTab.lyrics;
  bool open = true;
  final List<String> imported = [];
  String? applied;

  /// False makes the persist fail, as a full disk / lost DB would.
  bool saveSucceeds = true;

  /// Holds the persist until completed, as a save queued behind others does.
  Completer<void>? saveGate;

  @override
  bool get lyricsOpen => open;

  @override
  RightTab get rightTab => tab;

  @override
  void toggleLyricsPanel() {}

  // The Notes tab with nothing selected, for tab-switch tests.
  @override
  AudioTrack? get selectedTrack => null;

  @override
  List<({String trackId, String trackName, PracticeNote note})> get allNotes =>
      const [];

  @override
  bool get hasLyrics => text.isNotEmpty;

  // The reader, for when the editor closes.
  @override
  List<LyricLine> get lyrics => [LyricLine(text: text)];

  @override
  final ValueNotifier<Duration> position = ValueNotifier(Duration.zero);

  @override
  int currentLyricIndex(int positionMs) => -1;

  @override
  LyricsMode get lyricsMode => mode;

  @override
  String? lyricsDraft;

  @override
  int lyricsEditSession = 0;

  @override
  void setLyricsMode(LyricsMode m) {
    // As the real VM: leaving the editor (Save, Cancel) drops its draft, and opening it starts
    // a new editing session.
    if (m == mode) return;
    if (m != LyricsMode.edit) lyricsDraft = null;
    if (m == LyricsMode.edit) lyricsEditSession++;
    mode = m;
  }

  @override
  void setLyricsFocus(bool on) {}

  @override
  String lyricsAsText() => text;

  @override
  Future<void> importLyricsFromFile(String path) async {
    imported.add(path);
    // Mimic the real VM: only a valid lyrics file replaces the lyrics; a
    // rejected drop (wrong type, unreadable, busy) toasts and changes nothing.
    if (path.endsWith('.lrc')) text = 'dropped line one\ndropped line two';
  }

  // What the sync view reads.
  @override
  int get syncIndex => 0;
  @override
  Future<void> stampNextLyric() async {}
  @override
  Future<void> stampLyricAt(int index) async {}

  @override
  Future<bool> applyEditedLyrics(String t) async {
    applied = t;
    await saveGate?.future;
    if (!saveSucceeds) return false;
    text = t;
    return true;
  }
}

/// Fake for the karaoke READER: a playhead and enough synced lines that most
/// of them sit far outside the viewport (and so are never built by the
/// ListView until scrolled to).
class _ReaderVm extends Fake implements SongPracticeDetailViewModel {
  @override
  final ValueNotifier<Duration> position = ValueNotifier(Duration.zero);

  @override
  List<LyricLine> lyrics = _timed(stepMs: 1000);

  // Same pick as the real VM: the synced line with the latest time at or
  // before the playhead.
  @override
  int currentLyricIndex(int positionMs) {
    var idx = -1;
    var best = -1;
    for (var i = 0; i < lyrics.length; i++) {
      final t = lyrics[i].timeMs;
      if (t != null && t <= positionMs && t >= best) {
        best = t;
        idx = i;
      }
    }
    return idx;
  }

  @override
  void seekToLyric(LyricLine line) {}

  @override
  void setLyricsFocus(bool on) {}
}

List<LyricLine> _timed({required int stepMs}) => [
  for (var i = 0; i < 80; i++) LyricLine(text: 'line $i', timeMs: i * stepMs),
];

Future<void> _pumpFocus(WidgetTester tester, _ReaderVm vm) async {
  tester.view.physicalSize = const Size(900, 600);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(
    MaterialApp(
      home: Scaffold(body: LyricsFocusOverlay(vm: vm)),
    ),
  );
  await tester.pumpAndSettle();
}

/// Whether [text] is laid out inside the reader's viewport — not merely
/// built in the list's off-screen cache extent.
bool _onScreen(WidgetTester tester, String text) {
  final line = find.text(text);
  if (line.evaluate().isEmpty) return false;
  return tester
      .getRect(find.byType(ListView))
      .contains(tester.getRect(line).center);
}

bool _highlighted(WidgetTester tester, String text) =>
    tester.widget<Text>(find.text(text)).style!.fontWeight == FontWeight.w700;

Future<void> _pumpPanel(
  WidgetTester tester,
  _FakeVm vm, {
  double textScale = 0.6,
}) {
  return tester.pumpWidget(
    MaterialApp(
      home: MediaQuery(
        // The em-wide test font overflows the fixed-width tab bar that real
        // fonts fit comfortably; scale text down — this test targets the
        // drop-to-editor routing, not tab-bar layout.
        data: MediaQueryData(textScaler: TextScaler.linear(textScale)),
        child: Scaffold(
          body: Align(
            alignment: Alignment.topLeft,
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
}

/// Feeds a file into the panel's DropTarget exactly as desktop_drop would.
Future<void> _drop(WidgetTester tester, String path) async {
  final finder = find.byType(DropTarget);
  // On the target itself: a drop is taken only where it can be seen.
  final at = tester.getCenter(finder);
  tester.widget<DropTarget>(finder).onDragDone!(
    DropDoneDetails(
      files: [DropItemFile(path)],
      localPosition: Offset.zero,
      globalPosition: at,
    ),
  );
  // Flush the async import and the editor re-seed.
  await tester.pump();
  await tester.pump();
}

void main() {
  // Regression guard: dropping a .lrc while the lyrics EDITOR was open used to
  // import into the VM but leave the editor's controller on its stale
  // snapshot — pressing Save then silently overwrote the just-imported
  // lyrics. A successful edit-mode drop must re-seed the open editor.
  testWidgets('a drop while editing re-seeds the editor with the import', (
    tester,
  ) async {
    final vm = _FakeVm();
    await _pumpPanel(tester, vm);

    // The user has unsaved edits in the open editor.
    await tester.enterText(find.byType(TextField), 'user typed draft');
    expect(
      tester.widget<TextField>(find.byType(TextField)).controller!.text,
      'user typed draft',
    );

    await _drop(tester, '/tmp/song.lrc');

    expect(vm.imported, ['/tmp/song.lrc']);
    expect(
      tester.widget<TextField>(find.byType(TextField)).controller!.text,
      'dropped line one\ndropped line two',
      reason:
          'the open editor must show the imported lyrics, not a stale '
          'snapshot that Save would write back over the import',
    );

    // Save now applies the imported content — no silent overwrite.
    await tester.tap(find.text('Save'));
    await tester.pump();
    expect(vm.applied, 'dropped line one\ndropped line two');
    expect(tester.takeException(), isNull);
  });

  // Regression guard: Save used to leave edit mode unconditionally, so a write
  // that failed (and reverted the model to disk) took the draft with it,
  // leaving only a toast behind.
  testWidgets('a failed save keeps the editor open on the draft', (
    tester,
  ) async {
    final vm = _FakeVm()..saveSucceeds = false;
    await _pumpPanel(tester, vm);

    await tester.enterText(find.byType(TextField), 'user typed draft');
    await tester.tap(find.text('Save'));
    await tester.pump();
    await tester.pump();

    expect(vm.applied, 'user typed draft');
    expect(
      vm.mode,
      LyricsMode.edit,
      reason: "a write that didn't land must not close the editor",
    );
    expect(
      tester.widget<TextField>(find.byType(TextField)).controller!.text,
      'user typed draft',
      reason: 'the unsaved draft must survive for a retry',
    );
    expect(tester.takeException(), isNull);
  });

  testWidgets('text typed while the save waits stays in the open editor', (
    tester,
  ) async {
    // Save closed the editor when its write landed, dropping whatever was
    // typed after the tap with nothing to show for it.
    final vm = _FakeVm()..saveGate = Completer<void>();
    await _pumpPanel(tester, vm);

    await tester.enterText(find.byType(TextField), 'first verse');
    await tester.tap(find.text('Save'));
    await tester.pump();
    await tester.enterText(find.byType(TextField), 'first verse\nsecond');
    vm.saveGate!.complete();
    await tester.pump();
    await tester.pump();

    expect(vm.applied, 'first verse');
    expect(vm.mode, LyricsMode.edit);
    expect(
      tester.widget<TextField>(find.byType(TextField)).controller!.text,
      'first verse\nsecond',
    );

    // Saved again with nothing typed after, it closes.
    await tester.tap(find.text('Save'));
    await tester.pump();
    await tester.pump();
    expect(vm.applied, 'first verse\nsecond');
    expect(vm.mode, LyricsMode.read);
  });

  testWidgets('a save that lands after the user moved on to Sync leaves Sync', (
    tester,
  ) async {
    // Save closed the editor when its write landed, so a user who had tapped
    // Sync meanwhile (the lyrics already showed) was thrown back to Read.
    // (The sync toolbar needs a smaller scale than the editor to fit the
    // em-wide test font.)
    final vm = _FakeVm()..saveGate = Completer<void>();
    await _pumpPanel(tester, vm, textScale: 0.5);
    await tester.enterText(find.byType(TextField), 'first verse');
    await tester.tap(find.text('Save'));
    await tester.pump();
    await tester.tap(find.byTooltip('Sync (tap-to-time)'));
    await _pumpPanel(tester, vm, textScale: 0.5); // the fake does not notify
    expect(vm.mode, LyricsMode.sync);
    vm.saveGate!.complete();
    await tester.pump();
    await tester.pump();
    expect(vm.mode, LyricsMode.sync);
  });

  testWidgets('a late save leaves an editor opened again in the meantime', (
    tester,
  ) async {
    // Save, then Sync, then Edit before the write landed: the old Save closed the editor the
    // user had just reopened.
    final vm = _FakeVm()..saveGate = Completer<void>();
    await _pumpPanel(tester, vm, textScale: 0.5);
    await tester.enterText(find.byType(TextField), 'first verse');
    await tester.tap(find.text('Save'));
    await tester.pump();
    vm.setLyricsMode(LyricsMode.sync);
    await _pumpPanel(tester, vm, textScale: 0.5);
    vm.setLyricsMode(LyricsMode.edit);
    await _pumpPanel(tester, vm, textScale: 0.5);
    vm.saveGate!.complete();
    await tester.pump();
    await tester.pump();
    expect(vm.mode, LyricsMode.edit);
  });

  // Regression guard: the edit lived only in the editor's own state, which a
  // tab switch or a collapse of the panel threw away. Back on Lyrics the
  // still-open editor re-seeded from the saved lyrics and the edit was gone.
  group('an unsaved edit', () {
    String editorText(WidgetTester tester) =>
        tester.widget<TextField>(find.byType(TextField)).controller!.text;

    testWidgets('survives a switch to another tab and back', (tester) async {
      final vm = _FakeVm();
      await _pumpPanel(tester, vm);
      await tester.enterText(find.byType(TextField), 'user typed draft');

      vm.tab = RightTab.notes;
      await _pumpPanel(tester, vm);
      expect(find.byType(TextField), findsNothing);
      vm.tab = RightTab.lyrics;
      await _pumpPanel(tester, vm);

      expect(editorText(tester), 'user typed draft');
      expect(vm.applied, isNull, reason: 'a tab switch saves nothing');
      expect(tester.takeException(), isNull);
    });

    testWidgets('survives collapsing the panel', (tester) async {
      final vm = _FakeVm();
      await _pumpPanel(tester, vm);
      await tester.enterText(find.byType(TextField), 'user typed draft');

      vm.open = false;
      await _pumpPanel(tester, vm);
      vm.open = true;
      await _pumpPanel(tester, vm);

      expect(editorText(tester), 'user typed draft');
    });

    testWidgets('is dropped by Cancel', (tester) async {
      final vm = _FakeVm();
      await _pumpPanel(tester, vm);
      await tester.enterText(find.byType(TextField), 'user typed draft');
      await tester.tap(find.text('Cancel'));
      await _pumpPanel(tester, vm);
      expect(find.byType(TextField), findsNothing);

      vm.mode = LyricsMode.edit; // open the editor again
      await _pumpPanel(tester, vm);
      expect(editorText(tester), 'old line');
      expect(vm.applied, isNull);
    });
  });

  testWidgets('a rejected drop leaves the in-progress edit untouched', (
    tester,
  ) async {
    final vm = _FakeVm();
    await _pumpPanel(tester, vm);

    await tester.enterText(find.byType(TextField), 'user typed draft');
    await _drop(tester, '/tmp/photo.png'); // the VM rejects non-lyrics files

    expect(vm.imported, ['/tmp/photo.png']);
    expect(
      tester.widget<TextField>(find.byType(TextField)).controller!.text,
      'user typed draft',
      reason: 'a failed import must not clobber what the user is typing',
    );
    expect(tester.takeException(), isNull);
  });

  // Regression guard: the reader only scrolled to a line that already had an
  // element, and ListView.builder only builds the lines near the viewport. A
  // seek far down the song named an unbuilt line, so nothing scrolled — and
  // every line after it was just as unbuilt, so the reader stayed parked for
  // the rest of the song.
  testWidgets('the karaoke reader follows the playhead after a seek', (
    tester,
  ) async {
    final vm = _ReaderVm();
    await _pumpFocus(tester, vm);
    vm.position.value = const Duration(milliseconds: 3000);
    await tester.pumpAndSettle();
    expect(_onScreen(tester, 'line 3'), isTrue);

    // A ruler click / transport seek to 0:50…
    vm.position.value = const Duration(milliseconds: 50000);
    await tester.pumpAndSettle();
    expect(_onScreen(tester, 'line 50'), isTrue);

    // …and playback carries on from there.
    for (var s = 51; s <= 55; s++) {
      vm.position.value = Duration(milliseconds: s * 1000);
      await tester.pumpAndSettle();
    }
    expect(_onScreen(tester, 'line 55'), isTrue);
    expect(tester.takeException(), isNull);
  });

  testWidgets('opening Focus mid-song reveals the active line', (tester) async {
    final vm = _ReaderVm()
      ..position.value = const Duration(milliseconds: 40000);
    await _pumpFocus(tester, vm);
    expect(_onScreen(tester, 'line 40'), isTrue);
    expect(_highlighted(tester, 'line 40'), isTrue);
    expect(tester.takeException(), isNull);
  });

  // The highlighted index belonged to the OLD lyrics: after an import or edit
  // under a paused playhead it lit whatever line now sat at that index, and
  // stayed there until the playhead next crossed a line.
  testWidgets('new lyrics re-pick (and reveal) the line under the playhead', (
    tester,
  ) async {
    final vm = _ReaderVm()..position.value = const Duration(milliseconds: 5000);
    await _pumpFocus(tester, vm);
    expect(_highlighted(tester, 'line 5'), isTrue);

    // Re-timed lyrics: 5 s is now line 50's.
    vm.lyrics = _timed(stepMs: 100);
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(body: LyricsFocusOverlay(vm: vm)),
      ),
    );
    await tester.pumpAndSettle();
    expect(_onScreen(tester, 'line 50'), isTrue);
    expect(_highlighted(tester, 'line 50'), isTrue);
    expect(tester.takeException(), isNull);
  });
}
