import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:forui/forui.dart';
import 'package:integration_test/integration_test.dart';
import 'package:media_kit/media_kit.dart';
import 'package:vocara/data/db/persistence.dart';
import 'package:vocara/data/services/source_separation_service.dart';
import 'package:vocara/ui/core/widgets/studio_button.dart';
import 'package:vocara/ui/home/widgets/home_screen.dart';
import 'package:vocara/ui/home/widgets/practice_project_card.dart';
import 'package:vocara/ui/settings/settings_screen.dart';
import 'package:vocara/ui/song_practice_detail/widgets/song_practice_detail_screen.dart';
import 'package:vocara/ui/technique_library/technique_library_screen.dart';

import 'support/e2e.dart';

/// UI-driven journeys that boot the REAL app tree on Home (no `START`
/// deep-link) and drive the top-level navigation surface with taps: the empty
/// state, the header affordances, opening a seeded practice into its detail
/// screen, and the round trips into Settings and the Technique library.
///
/// Run with:
///   flutter test integration_test/home_settings_journey_test.dart -d macos
///
/// Every wait is a bounded [pumpUntil] asserted `isTrue` so a timeout reads as a
/// clear expectation failure, never a hang. Home / Settings / library carry no
/// indefinite painters, but the detail screen does — so we never `pumpAndSettle`
/// once a journey crosses into it (and Settings runs a 1.5 s device-poll timer
/// that would also defeat `pumpAndSettle`).
void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  // Opening a practice constructs a media_kit-backed PlaybackEngine in the
  // detail workspace; initialize the library so that build is safe.
  MediaKit.ensureInitialized();

  late Directory tmpDir;
  late Persistence persistence;

  setUp(() {
    tmpDir = Directory.systemTemp.createTempSync('vocara_home_e2e');
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

  Future<void> boot(WidgetTester tester) =>
      pumpApp(tester, persistence, separation: FakeSourceSeparationService());

  testWidgets('empty store lands on Home with the empty state + header nav', (
    tester,
  ) async {
    await boot(tester);

    expect(
      await pumpUntil(tester, () => present(find.byType(HomeScreen))),
      isTrue,
      reason: 'a no-START boot lands on Home',
    );
    expect(
      await pumpUntil(tester, () => present(find.text('No practices yet'))),
      isTrue,
      reason: 'a fresh in-memory store shows the empty state',
    );
    expect(
      find.byType(PracticeProjectCard),
      findsNothing,
      reason: 'no practices means no cards',
    );

    // The three header affordances are always present on Home.
    expect(find.widgetWithText(StudioButton, 'New practice'), findsOneWidget);
    expect(find.widgetWithText(StudioButton, 'Techniques'), findsOneWidget);
    expect(
      find.descendant(
        of: find.byType(HomeScreen),
        matching: find.byIcon(FIcons.settings),
      ),
      findsOneWidget,
      reason: 'the header exposes a settings entry',
    );
  });

  testWidgets('seeded practices render as cards; tapping one opens detail then '
      'the back chevron returns Home', (tester) async {
    // Seed two practices BEFORE the tree builds, so HomeViewModel.load (which
    // runs on construction) sees them.
    await persistence.songRepo.createPractice('Bohemian Rhapsody');
    await persistence.songRepo.createPractice('Hai Kuo Tian Kong');

    await boot(tester);
    expect(
      await pumpUntil(tester, () => present(find.byType(HomeScreen))),
      isTrue,
    );

    // Both practices land on the grid.
    expect(
      await pumpUntil(
        tester,
        () => find.byType(PracticeProjectCard).evaluate().length == 2,
      ),
      isTrue,
      reason: 'two seeded practices render two cards',
    );
    expect(
      find.widgetWithText(PracticeProjectCard, 'Bohemian Rhapsody'),
      findsOneWidget,
    );
    expect(
      find.widgetWithText(PracticeProjectCard, 'Hai Kuo Tian Kong'),
      findsOneWidget,
    );

    // Open one card. The home screen shows a ~1.5 s _OpeningDialog before
    // pushing the detail route, so pump (capped) through it — never settle.
    await tester.tap(
      find.widgetWithText(PracticeProjectCard, 'Bohemian Rhapsody'),
    );
    expect(
      await pumpUntil(
        tester,
        () => present(find.byType(SongPracticeDetailScreen)),
        maxIterations: 60,
      ),
      isTrue,
      reason: 'tapping a card navigates into its detail screen',
    );
    expect(
      await pumpUntil(tester, () => present(find.text('Bohemian Rhapsody'))),
      isTrue,
      reason: 'the detail transport bar shows the opened practice title',
    );

    // The transport back chevron returns Home.
    await tester.tap(find.byIcon(FIcons.chevronLeft).first);
    expect(
      await pumpUntil(
        tester,
        () =>
            present(find.byType(HomeScreen)) &&
            !present(find.byType(SongPracticeDetailScreen)),
      ),
      isTrue,
      reason: 'the back chevron returns to Home',
    );
  });

  testWidgets('Home → Settings → back round-trips through the settings screen', (
    tester,
  ) async {
    await boot(tester);
    expect(
      await pumpUntil(tester, () => present(find.byType(HomeScreen))),
      isTrue,
    );

    await tester.tap(
      find.descendant(
        of: find.byType(HomeScreen),
        matching: find.byIcon(FIcons.settings),
      ),
    );
    expect(
      await pumpUntil(tester, () => present(find.byType(SettingsScreen))),
      isTrue,
      reason: 'the settings icon pushes the settings route',
    );

    // The settings surface lays out its device + monitoring sections. Device
    // lists are environment-dependent, so assert only the always-present rows.
    expect(find.text('Settings'), findsOneWidget);
    expect(find.text('AUDIO OUTPUT'), findsOneWidget);
    expect(find.text('AUDIO INPUT'), findsOneWidget);
    expect(find.text('MONITORING'), findsOneWidget);
    expect(
      find.text('System Default'),
      findsWidgets,
      reason: 'each device card offers a System Default row',
    );
    expect(
      find.byType(Slider),
      findsOneWidget,
      reason: 'the monitoring reverb control renders a slider',
    );

    // Back chevron (scoped to settings) returns Home.
    await tester.tap(
      find.descendant(
        of: find.byType(SettingsScreen),
        matching: find.byIcon(FIcons.chevronLeft),
      ),
    );
    expect(
      await pumpUntil(
        tester,
        () =>
            present(find.byType(HomeScreen)) &&
            !present(find.byType(SettingsScreen)),
      ),
      isTrue,
      reason: 'the settings back chevron pops to Home',
    );
  });

  testWidgets(
    'Home → Technique library → back round-trips through the library',
    (tester) async {
      await boot(tester);
      expect(
        await pumpUntil(tester, () => present(find.byType(HomeScreen))),
        isTrue,
      );

      await tester.tap(find.widgetWithText(StudioButton, 'Techniques'));
      expect(
        await pumpUntil(
          tester,
          () => present(find.byType(TechniqueLibraryScreen)),
        ),
        isTrue,
        reason: 'the Techniques header button pushes the library route',
      );
      expect(find.text('Technique library'), findsOneWidget);

      await tester.tap(
        find.descendant(
          of: find.byType(TechniqueLibraryScreen),
          matching: find.byIcon(FIcons.chevronLeft),
        ),
      );
      expect(
        await pumpUntil(
          tester,
          () =>
              present(find.byType(HomeScreen)) &&
              !present(find.byType(TechniqueLibraryScreen)),
        ),
        isTrue,
        reason: 'the library back chevron pops to Home',
      );
    },
  );

  testWidgets('hovering a card reveals its delete affordance; confirming '
      'removes it and the empty state returns', (tester) async {
    await persistence.songRepo.createPractice('Doomed practice');

    await boot(tester);
    expect(
      await pumpUntil(tester, () => present(find.byType(HomeScreen))),
      isTrue,
    );
    expect(
      await pumpUntil(tester, () => present(find.byType(PracticeProjectCard))),
      isTrue,
      reason: 'the seeded practice renders a card',
    );

    // The delete icon only builds while the card is hovered.
    await hoverOver(tester, find.byType(PracticeProjectCard));
    expect(
      await pumpUntil(tester, () => present(find.byIcon(FIcons.trash2))),
      isTrue,
      reason: 'hovering a card reveals its trash affordance',
    );

    await tester.tap(find.byIcon(FIcons.trash2));
    expect(
      await pumpUntil(tester, () => present(find.textContaining('Delete'))),
      isTrue,
      reason: 'tapping trash opens a delete-confirmation dialog',
    );

    // Confirm the deletion.
    await tester.tap(find.widgetWithText(StudioButton, 'Delete'));
    expect(
      await pumpUntil(
        tester,
        () =>
            !present(find.byType(PracticeProjectCard)) &&
            present(find.text('No practices yet')),
      ),
      isTrue,
      reason: 'confirming delete removes the card and restores the empty state',
    );
  });
}
