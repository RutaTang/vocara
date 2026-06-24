import 'package:flutter_test/flutter_test.dart';
import 'package:provider/provider.dart';
import 'package:vocara/app.dart';
import 'package:vocara/config/dependencies.dart';
import 'package:vocara/data/db/persistence.dart';

void main() {
  testWidgets('App boots to the home screen', (tester) async {
    await tester.pumpWidget(
      MultiProvider(
        providers: buildProviders(Persistence.memory()),
        child: const VocaraApp(),
      ),
    );
    await tester.pump();

    // The home header title is rendered.
    expect(find.text('Vocara'), findsOneWidget);
  });
}
