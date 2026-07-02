import 'package:desktop_drop/desktop_drop.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/ui/song_practice_detail/view_model/song_practice_detail_view_model.dart';
import 'package:vocara/ui/song_practice_detail/widgets/lyrics_panel.dart';

/// Minimal fake: only the members RightPanel touches with the lyrics tab open
/// in EDIT mode, plus the import/apply hooks the drop flow drives.
class _FakeVm extends Fake implements SongPracticeDetailViewModel {
  String text = 'old line';
  LyricsMode mode = LyricsMode.edit;
  final List<String> imported = [];
  String? applied;

  @override
  bool get lyricsOpen => true;

  @override
  RightTab get rightTab => RightTab.lyrics;

  @override
  void toggleLyricsPanel() {}

  @override
  bool get hasLyrics => text.isNotEmpty;

  @override
  LyricsMode get lyricsMode => mode;

  @override
  void setLyricsMode(LyricsMode m) => mode = m;

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

  @override
  Future<void> applyEditedLyrics(String t) async {
    applied = t;
    text = t;
  }
}

Future<void> _pumpPanel(WidgetTester tester, _FakeVm vm) {
  return tester.pumpWidget(
    MaterialApp(
      home: MediaQuery(
        // The em-wide test font overflows the fixed-width tab bar that real
        // fonts fit comfortably; scale text down — this test targets the
        // drop-to-editor routing, not tab-bar layout.
        data: const MediaQueryData(textScaler: TextScaler.linear(0.6)),
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
  final target = tester.widget<DropTarget>(find.byType(DropTarget));
  target.onDragDone!(
    DropDoneDetails(
      files: [DropItemFile(path)],
      localPosition: Offset.zero,
      globalPosition: Offset.zero,
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
    expect(tester.widget<TextField>(find.byType(TextField)).controller!.text,
        'user typed draft');

    await _drop(tester, '/tmp/song.lrc');

    expect(vm.imported, ['/tmp/song.lrc']);
    expect(
      tester.widget<TextField>(find.byType(TextField)).controller!.text,
      'dropped line one\ndropped line two',
      reason: 'the open editor must show the imported lyrics, not a stale '
          'snapshot that Save would write back over the import',
    );

    // Save now applies the imported content — no silent overwrite.
    await tester.tap(find.text('Save'));
    await tester.pump();
    expect(vm.applied, 'dropped line one\ndropped line two');
    expect(tester.takeException(), isNull);
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
}
