import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:media_kit/media_kit.dart';
import 'package:vocara/data/db/persistence.dart';
import 'package:vocara/data/services/source_separation_service.dart';
import 'package:vocara/ui/settings/settings_screen.dart';

import 'support/e2e.dart';

/// UI-driven journeys for the global Settings screen: it lays out its device +
/// monitoring sections, and the monitoring reverb `Slider` responds to a drag.
///
/// Run with the settings deep-link:
///   flutter test integration_test/settings_journey_test.dart -d macos \
///       --dart-define=START=/settings
///
/// Settings carries no indefinite painters but DOES run a 1.5 s device-poll
/// timer, so we use bounded [pumpUntil] (never `pumpAndSettle`). Enumerated
/// audio devices are environment-dependent, so we assert only the always-present
/// rows and drive the reverb slider by relative change (not an absolute value).
void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  MediaKit.ensureInitialized();

  late Directory tmpDir;
  late Persistence persistence;

  setUp(() {
    tmpDir = Directory.systemTemp.createTempSync('vocara_settings_e2e');
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

  Future<void> boot(WidgetTester tester) async {
    await pumpApp(tester, persistence, separation: FakeSourceSeparationService());
    expect(
      await pumpUntil(tester, () => present(find.byType(SettingsScreen))),
      isTrue,
      reason:
          'should deep-link into the settings screen — run with '
          '--dart-define=START=/settings',
    );
  }

  /// The current monitoring-reverb percentage text (e.g. "20%"). The reverb
  /// readout is the only `\d+%` text on the screen.
  String reverbPercent(WidgetTester tester) {
    final finder = find.byWidgetPredicate(
      (w) => w is Text && w.data != null && RegExp(r'^\d+%$').hasMatch(w.data!),
    );
    return tester.widget<Text>(finder).data!;
  }

  testWidgets('settings lays out its device + monitoring sections', (
    tester,
  ) async {
    await boot(tester);

    expect(await pumpUntil(tester, () => present(find.text('Settings'))), isTrue);
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
  });

  testWidgets('dragging the reverb slider changes its percentage readout', (
    tester,
  ) async {
    await boot(tester);

    final slider = find.byType(Slider);
    expect(await pumpUntil(tester, () => present(slider)), isTrue);

    final before = reverbPercent(tester);
    // Drag right: a Material slider seeks to the press point then follows, so
    // the wet level (and its readout) must end up different from the start.
    await tester.drag(slider, const Offset(80, 0));
    expect(
      await pumpUntil(tester, () => reverbPercent(tester) != before),
      isTrue,
      reason: 'dragging the reverb slider updates the percentage readout',
    );
  });
}
