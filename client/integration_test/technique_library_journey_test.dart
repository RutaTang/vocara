import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:forui/forui.dart';
import 'package:integration_test/integration_test.dart';
import 'package:media_kit/media_kit.dart';
import 'package:vocara/data/db/persistence.dart';
import 'package:vocara/data/services/source_separation_service.dart';
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/ui/core/widgets/studio_button.dart';
import 'package:vocara/ui/technique_library/technique_library_screen.dart';
import 'package:vocara/ui/technique_library/widgets/studio_field.dart';
import 'package:vocara/ui/technique_library/widgets/technique_detail_screen.dart';
import 'package:vocara/utils/id.dart';
import 'package:vocara/utils/result.dart';

import 'support/e2e.dart';

/// UI-driven journeys for the technique LIBRARY list surface: the empty state,
/// creating a technique through the dialog (into its detail and back to the
/// grid), and the search filter over a seeded grid. The tag filter is covered
/// by the widget tests (`test/technique_library_layout_test.dart`), not here.
/// The library list carries no indefinite painters, so these could
/// `pumpAndSettle`, but we keep bounded [pumpUntil] for consistency.
///
/// Run with the library deep-link:
///   flutter test integration_test/technique_library_journey_test.dart -d macos \
///       --dart-define=START=/techniques
void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  MediaKit.ensureInitialized();

  late Directory tmpDir;
  late Persistence persistence;

  setUp(() {
    tmpDir = Directory.systemTemp.createTempSync('vocara_library_e2e');
    installE2eChannelMocks(tmpDir);
    persistence = Persistence.memory();
  });

  tearDown(() async {
    removeE2eChannelMocks();
    await persistence.db.close();
    try {
      tmpDir.deleteSync(recursive: true);
    } catch (_) {
      // Best-effort cleanup; a leftover temp dir must not fail the test.
    }
  });

  /// Sizes the test window large enough for the card grid + filter bar to lay
  /// out without overflow, then pumps the real tree at the library deep-link.
  Future<void> boot(WidgetTester tester) async {
    tester.view.physicalSize = const Size(1440, 1000);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);

    await pumpApp(
      tester,
      persistence,
      separation: FakeSourceSeparationService(),
    );
    expect(
      await pumpUntil(
        tester,
        () => present(find.byType(TechniqueLibraryScreen)),
      ),
      isTrue,
      reason:
          'should deep-link into the technique library — run with '
          '--dart-define=START=/techniques',
    );
  }

  Future<void> seedTech(String name, {List<String> tags = const []}) async {
    final now = DateTime.now();
    await persistence.techRepo.saveTechnique(
      Technique(
        id: newId(),
        name: name,
        tags: tags,
        createdAt: now,
        updatedAt: now,
      ),
    );
  }

  testWidgets('an empty library shows the build-your-library empty state', (
    tester,
  ) async {
    await boot(tester);
    expect(
      await pumpUntil(
        tester,
        () => present(find.text('Build your technique library')),
      ),
      isTrue,
      reason: 'a fresh store shows the library empty state',
    );
  });

  testWidgets('creating a technique navigates to its detail, then it shows in '
      'the grid on return', (tester) async {
    await boot(tester);
    expect(
      await pumpUntil(
        tester,
        () => present(find.text('Build your technique library')),
      ),
      isTrue,
    );

    // Open the create dialog and name the technique.
    await tester.tap(find.widgetWithText(StudioButton, 'New technique'));
    expect(
      await pumpUntil(tester, () => present(find.byType(StudioField))),
      isTrue,
      reason: 'the New technique button opens the create dialog',
    );
    await tester.enterText(
      find.descendant(
        of: find.byType(StudioField),
        matching: find.byType(TextField),
      ),
      'Twang',
    );
    // Create enables on the frame after the name lands: a tap before it hits
    // the disabled button and is dropped.
    await tester.pump();
    await tester.tap(find.widgetWithText(StudioButton, 'Create'));

    // Creating navigates straight into the new technique's detail screen.
    expect(
      await pumpUntil(
        tester,
        () => present(find.byType(TechniqueDetailScreen)),
      ),
      isTrue,
      reason: 'a created technique opens its detail screen',
    );
    expect(
      await pumpUntil(tester, () => present(find.text('Twang'))),
      isTrue,
      reason: 'the new technique loads with its name',
    );

    // Back to the library — the new technique now shows as a card.
    await tester.tap(
      find.descendant(
        of: find.byType(TechniqueDetailScreen),
        matching: find.byIcon(FIcons.chevronLeft),
      ),
    );
    expect(
      await pumpUntil(
        tester,
        () =>
            present(find.byType(TechniqueLibraryScreen)) &&
            !present(find.byType(TechniqueDetailScreen)),
      ),
      isTrue,
      reason: 'the detail back chevron returns to the library',
    );
    expect(
      await pumpUntil(tester, () => present(find.text('Twang'))),
      isTrue,
      reason: 'the created technique appears in the grid',
    );

    // And it persisted through the repo the test owns.
    final all =
        (await persistence.techRepo.listTechniques() as Ok<List<Technique>>)
            .value;
    expect(all.map((t) => t.name), contains('Twang'));
  });

  testWidgets('the search field filters the grid by name', (tester) async {
    await seedTech('Twang');
    await seedTech('Curbing');

    await boot(tester);
    expect(
      await pumpUntil(tester, () => present(find.text('Twang'))),
      isTrue,
      reason: 'seeded techniques render as cards',
    );
    expect(find.text('Curbing'), findsWidgets);

    await tester.enterText(find.byType(TextField).first, 'twa');
    expect(
      await pumpUntil(
        tester,
        () => present(find.text('Twang')) && !present(find.text('Curbing')),
      ),
      isTrue,
      reason: 'a name query keeps the match and hides the rest',
    );
  });
}
