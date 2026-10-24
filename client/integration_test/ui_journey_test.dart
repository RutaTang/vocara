import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:forui/forui.dart';
import 'package:integration_test/integration_test.dart';
import 'package:media_kit/media_kit.dart';
import 'package:provider/provider.dart';
import 'package:vocara/app.dart';
import 'package:vocara/config/dependencies.dart';
import 'package:vocara/data/db/persistence.dart';
import 'package:vocara/ui/home/widgets/home_screen.dart';
import 'package:vocara/ui/home/widgets/practice_project_card.dart';
import 'package:vocara/ui/song_practice_detail/widgets/song_practice_detail_screen.dart';

/// A UI-driven, end-to-end journey that pumps the REAL widget tree (the same
/// `MultiProvider(buildProviders(...))` + `VocaraApp` that `main.dart` builds)
/// and simulates user taps: land on Home, create a practice through the dialog,
/// confirm we navigate into the detail screen, then go back and confirm the new
/// practice shows up as a card.
///
/// Uses `Persistence.memory()` so there is no on-disk state. Platform channels
/// the tree might brush against before/around the first frame (path_provider,
/// window_manager) are mocked with sensible defaults so a pump never throws even
/// if the native plugin registration timing differs; on the real macOS embedder
/// these are normally already satisfied.
void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  // main() initializes media_kit (it backs the technique example players)
  // before the tree builds; mirror it so this boots the tree main() boots.
  // The song detail itself plays through the native engine.
  MediaKit.ensureInitialized();

  final messenger =
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;

  // A temp dir handed back to any path_provider query so LocalStorageService /
  // AppSettingsService.load() resolve a real, writable directory without
  // depending on the plugin's native registration.
  late Directory tmpDir;

  setUp(() {
    tmpDir = Directory.systemTemp.createTempSync('vocara_ui_journey');

    // path_provider: every "get …Directory" returns our temp dir.
    messenger.setMockMethodCallHandler(
      const MethodChannel('plugins.flutter.io/path_provider'),
      (call) async {
        if (call.method.startsWith('get')) return tmpDir.path;
        return null;
      },
    );
    // Older path_provider channel name (kept in case the plugin uses it).
    messenger.setMockMethodCallHandler(
      const MethodChannel('plugins.flutter.io/path_provider_macos'),
      (call) async {
        if (call.method.startsWith('get')) return tmpDir.path;
        return null;
      },
    );

    // window_manager: the WindowDragBar's DragToMoveArea is built on every page.
    // It only invokes the channel from gesture callbacks (drag/double-tap),
    // which this journey never triggers, but mock it defensively with safe
    // defaults so any incidental call resolves rather than throwing.
    messenger.setMockMethodCallHandler(const MethodChannel('window_manager'), (
      call,
    ) async {
      switch (call.method) {
        case 'isMaximized':
        case 'isMinimized':
        case 'isFullScreen':
        case 'isPreventClose':
        case 'isVisible':
        case 'isFocused':
          return false;
        default:
          return null;
      }
    });

    // The native window reveal channel main.dart pings after the first frame.
    messenger.setMockMethodCallHandler(
      const MethodChannel('vocara/window'),
      (call) async => null,
    );
  });

  tearDown(() {
    messenger.setMockMethodCallHandler(
      const MethodChannel('plugins.flutter.io/path_provider'),
      null,
    );
    messenger.setMockMethodCallHandler(
      const MethodChannel('plugins.flutter.io/path_provider_macos'),
      null,
    );
    messenger.setMockMethodCallHandler(
      const MethodChannel('window_manager'),
      null,
    );
    messenger.setMockMethodCallHandler(
      const MethodChannel('vocara/window'),
      null,
    );
    try {
      tmpDir.deleteSync(recursive: true);
    } catch (_) {
      // Best-effort cleanup; a leftover temp dir must not fail the test.
    }
  });

  /// Pumps repeatedly (capped) until [condition] holds or the cap is hit.
  /// Returns whether the condition was satisfied — callers assert on it so a
  /// timeout surfaces as a clear expectation failure rather than a hang.
  Future<bool> pumpUntil(
    WidgetTester tester,
    bool Function() condition, {
    int maxIterations = 60,
    Duration step = const Duration(milliseconds: 100),
  }) async {
    for (var i = 0; i < maxIterations; i++) {
      if (condition()) return true;
      await tester.pump(step);
    }
    return condition();
  }

  testWidgets('home → new-practice dialog → detail → back to a card', (
    tester,
  ) async {
    final persistence = Persistence.memory();
    addTearDown(() => persistence.db.close());

    await tester.pumpWidget(
      MultiProvider(
        providers: buildProviders(persistence),
        child: const VocaraApp(),
      ),
    );

    // Let the router build Home and HomeViewModel.load (runs on construction)
    // settle. With a fresh in-memory store the list is empty, so the empty
    // state should appear.
    final landed = await pumpUntil(
      tester,
      () => find.byType(HomeScreen).evaluate().isNotEmpty,
    );
    expect(landed, isTrue, reason: 'should land on the home screen');

    // Empty store → the "No practices yet" empty state, and no project cards.
    await pumpUntil(
      tester,
      () => find.text('No practices yet').evaluate().isNotEmpty,
    );
    expect(
      find.byType(PracticeProjectCard),
      findsNothing,
      reason: 'a fresh in-memory store has no practices',
    );

    // Open the "New practice" dialog from the header button.
    expect(
      find.text('New practice'),
      findsOneWidget,
      reason: 'the header exposes a single New practice affordance',
    );
    await tester.tap(find.text('New practice'));
    await tester.pumpAndSettle();

    // The studio prompt dialog should be up (title + a text field + Create).
    expect(find.text('New song practice'), findsOneWidget);
    expect(find.byType(TextField), findsOneWidget);
    expect(find.text('Create'), findsOneWidget);

    // Type a title and confirm.
    const title = 'Bohemian Rhapsody';
    await tester.enterText(find.byType(TextField), title);
    await tester.pump();
    await tester.tap(find.text('Create'));
    // The create flow then shows an _OpeningDialog for ~1.5s before pushing the
    // detail route, so settle through that delay (capped) until the detail
    // screen mounts.
    final reachedDetail = await pumpUntil(
      tester,
      () => find.byType(SongPracticeDetailScreen).evaluate().isNotEmpty,
      maxIterations: 80,
      step: const Duration(milliseconds: 100),
    );
    // Don't pumpAndSettle here: the detail workspace can carry indefinite
    // animations (e.g. a loading spinner / live painters) that never settle.
    // Pump until the practice has loaded (its title is shown) instead.
    await pumpUntil(
      tester,
      () => find.text(title).evaluate().isNotEmpty,
      maxIterations: 40,
    );

    expect(
      reachedDetail,
      isTrue,
      reason: 'confirming the dialog should navigate into the detail screen',
    );
    expect(find.byType(SongPracticeDetailScreen), findsOneWidget);
    // The detail transport bar renders the practice title once loaded.
    expect(
      find.text(title),
      findsWidgets,
      reason: 'the detail view shows the new practice title',
    );

    // Navigate back to Home via the detail transport bar's back chevron, which
    // calls context.go(Routes.home).
    final backButton = find.byIcon(FIcons.chevronLeft);
    expect(backButton, findsWidgets);
    await tester.tap(backButton.first);
    // context.go replaces the route; pump (bounded) until Home re-mounts rather
    // than pumpAndSettle, to avoid hanging on any in-flight transition/painter.
    final backHome = await pumpUntil(
      tester,
      () =>
          find.byType(HomeScreen).evaluate().isNotEmpty &&
          find.byType(SongPracticeDetailScreen).evaluate().isEmpty,
    );
    expect(backHome, isTrue, reason: 'back chevron returns to home');

    final cardAppeared = await pumpUntil(
      tester,
      () => find.byType(PracticeProjectCard).evaluate().isNotEmpty,
    );
    expect(
      cardAppeared,
      isTrue,
      reason: 'the created practice should appear on the home grid',
    );
    expect(
      find.widgetWithText(PracticeProjectCard, title),
      findsOneWidget,
      reason: 'the card shows the practice title',
    );
  });
}
