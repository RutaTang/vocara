// The minimal fatal-open window main() shows when Persistence.open throws:
// the newer-library refusal and the generic open failure, each with working
// actions. Rendered through FatalOpenApp, the exact wrapper main() uses.

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/ui/core/widgets/fatal_open_screen.dart';

void main() {
  testWidgets('newer-library window shows the update copy and quits', (
    tester,
  ) async {
    var quits = 0;
    await tester.pumpWidget(
      FatalOpenApp(screen: FatalOpenScreen.newerLibrary(onQuit: () => quits++)),
    );

    expect(
      find.text('This library was created by a newer version of Vocara.'),
      findsOneWidget,
    );
    expect(find.text('Please update Vocara to open it.'), findsOneWidget);
    // The data is fine and no folder action is offered, only quitting.
    expect(find.text('Reveal Library Folder'), findsNothing);

    await tester.tap(find.text('Quit Vocara'));
    expect(quits, 1);
  });

  testWidgets('open-failed window shows the error detail and both actions '
      'work', (tester) async {
    var quits = 0;
    var reveals = 0;
    await tester.pumpWidget(
      FatalOpenApp(
        screen: FatalOpenScreen.openFailed(
          detail: 'SqliteException(11): database disk image is malformed',
          onRevealLibrary: () => reveals++,
          onQuit: () => quits++,
        ),
      ),
    );

    expect(find.text('Vocara could not open its library.'), findsOneWidget);
    expect(
      find.textContaining('database disk image is malformed'),
      findsOneWidget,
    );

    await tester.tap(find.text('Reveal Library Folder'));
    await tester.tap(find.text('Quit Vocara'));
    expect(reveals, 1);
    expect(quits, 1);
  });
}
