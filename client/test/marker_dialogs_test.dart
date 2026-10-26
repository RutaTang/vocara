// The note editor and the technique picker: what they let through, and what
// they say.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/domain/models/practice_note.dart';
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/ui/song_practice_detail/widgets/note_editor.dart';
import 'package:vocara/ui/song_practice_detail/widgets/technique_picker.dart';

void main() {
  /// Opens [show] from a button and returns where its result lands.
  Future<List<Object?>> open(
    WidgetTester tester,
    Future<Object?> Function(BuildContext) show,
  ) async {
    final results = <Object?>[];
    await tester.pumpWidget(
      MaterialApp(
        home: Builder(
          builder: (context) => TextButton(
            onPressed: () async => results.add(await show(context)),
            child: const Text('open'),
          ),
        ),
      ),
    );
    await tester.tap(find.text('open'));
    await tester.pumpAndSettle();
    return results;
  }

  testWidgets('a note with no text cannot be added or saved', (tester) async {
    // Empty, "Save" closed as if it had saved, the note keeping its old text.
    final results = await open(
      tester,
      (context) => showNoteEditor(
        context,
        at: Duration.zero,
        existing: PracticeNote(
          id: 'n',
          positionMs: 0,
          content: 'breath',
          createdAt: DateTime(2026),
          updatedAt: DateTime(2026),
        ),
      ),
    );
    await tester.enterText(find.byType(TextField), '   ');
    await tester.pump();
    await tester.tap(find.text('Save'));
    await tester.pumpAndSettle();
    expect(results, isEmpty, reason: 'the dialog stays open');
    expect(find.text('Save'), findsOneWidget);

    await tester.enterText(find.byType(TextField), 'sharp');
    await tester.pump();
    await tester.tap(find.text('Save'));
    await tester.pumpAndSettle();
    expect((results.single as NoteEditorResult?)?.content, 'sharp');
  });

  testWidgets('a drag too short for a range is titled as the point it is '
      'saved as', (tester) async {
    await open(
      tester,
      (context) => showNoteEditor(
        context,
        at: const Duration(milliseconds: 12000),
        end: const Duration(milliseconds: 12050),
      ),
    );
    expect(find.text('Note at 00:12'), findsOneWidget);
  });

  testWidgets('a library that could not be read is not called empty', (
    tester,
  ) async {
    await open(
      tester,
      (context) =>
          showTechniquePicker(context, Future<List<Technique>?>.value(null)),
    );
    expect(
      find.textContaining("Couldn't open the technique library"),
      findsOneWidget,
    );
    expect(find.textContaining('No techniques yet'), findsNothing);
  });
}
