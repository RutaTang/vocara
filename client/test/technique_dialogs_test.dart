import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/ui/technique_library/widgets/technique_dialogs.dart';

void main() {
  testWidgets('Return in the name field creates the technique', (tester) async {
    String? result;
    var closed = false;
    await tester.pumpWidget(
      MaterialApp(
        home: Builder(
          builder: (context) => TextButton(
            onPressed: () async {
              result = await showNewTechniqueDialog(context);
              closed = true;
            },
            child: const Text('open'),
          ),
        ),
      ),
    );
    await tester.tap(find.text('open'));
    await tester.pumpAndSettle();

    await tester.enterText(find.byType(TextField), '  Twang ');
    await tester.testTextInput.receiveAction(TextInputAction.done);
    await tester.pumpAndSettle();

    expect(closed, isTrue, reason: 'Return closes the dialog');
    expect(result, 'Twang');
  });

  testWidgets('a blank name creates nothing and keeps the dialog open', (
    tester,
  ) async {
    // Closed, it created nothing and said nothing.
    var closed = false;
    await tester.pumpWidget(
      MaterialApp(
        home: Builder(
          builder: (context) => TextButton(
            onPressed: () async {
              await showNewTechniqueDialog(context);
              closed = true;
            },
            child: const Text('open'),
          ),
        ),
      ),
    );
    await tester.tap(find.text('open'));
    await tester.pumpAndSettle();

    await tester.enterText(find.byType(TextField), '   ');
    await tester.testTextInput.receiveAction(TextInputAction.done);
    await tester.pumpAndSettle();
    await tester.tap(find.text('Create'));
    await tester.pumpAndSettle();
    expect(closed, isFalse);
    expect(find.text('New technique'), findsOneWidget);
  });
}
