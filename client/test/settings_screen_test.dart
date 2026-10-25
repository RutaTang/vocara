// Settings' maintenance + about surfaces: the Repair-library flow (confirm ->
// Persistence.repairLibrary() -> report toast), the diagnostics export (save
// panel seam -> bundle on disk -> toast), the Check-for-updates row (manual
// check, result line, download dialog), and the hand-maintained license
// registration. Device enumeration and all network/pickers are injected.

import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:provider/provider.dart';
import 'package:vocara/data/db/persistence.dart';
import 'package:vocara/data/services/app_settings_service.dart';
import 'package:vocara/data/services/diagnostics_log.dart';
import 'package:vocara/data/services/graph_audio_playback_service.dart';
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/data/services/update_check_service.dart';
import 'package:vocara/domain/models/audio_device.dart';
import 'package:vocara/ui/settings/licenses.dart';
import 'package:vocara/ui/settings/settings_screen.dart';

Future<Persistence> _pumpSettings(
  WidgetTester tester, {
  UpdateCheckService? updates,
  Future<String?> Function(String suggestedName)? pickSavePath,
  Future<String> Function()? exportDiagnostics,
  AppSettingsService? settings,
}) async {
  // Tall window so every settings section is on screen without scrolling.
  tester.view.physicalSize = const Size(1400, 2600);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.reset);

  final persistence = Persistence.memory();
  settings ??= AppSettingsService(
    storage: LocalStorageService(),
    enumerateOutputs: () => const [],
    enumerateInputs: () => const [],
    inputChannelCount: (_) => 0,
  );
  await tester.pumpWidget(
    MultiProvider(
      providers: [
        ChangeNotifierProvider<AppSettingsService>.value(value: settings),
        Provider<Persistence>.value(value: persistence),
        ChangeNotifierProvider<UpdateCheckService>.value(
          value:
              updates ??
              UpdateCheckService(
                fetch: (_) async =>
                    '{"latest": "1.0.0", "url": "https://example.com"}',
                currentVersion: '1.0.0',
              ),
        ),
      ],
      child: MaterialApp(
        home: SettingsScreen(
          pickSavePath: pickSavePath,
          exportDiagnostics: exportDiagnostics,
        ),
      ),
    ),
  );
  await tester.pump();
  return persistence;
}

/// Pumps while letting REAL async work (file/database IO) complete, until
/// [finder] matches; fails the test after [maxTries].
Future<void> _pumpUntilFound(
  WidgetTester tester,
  Finder finder, {
  int maxTries = 200,
}) async {
  for (var i = 0; i < maxTries; i++) {
    if (finder.evaluate().isNotEmpty) return;
    await tester.runAsync(
      () => Future<void>.delayed(const Duration(milliseconds: 20)),
    );
    await tester.pump();
  }
  expect(finder, findsOneWidget);
}

/// Unmounts the tree so the screen's device-poll timer is cancelled, then
/// closes the store outside the fake-async zone.
Future<void> _teardown(WidgetTester tester, Persistence persistence) async {
  await tester.pumpWidget(const SizedBox());
  await tester.runAsync(() => persistence.db.close());
  final dir = persistence.assets.root;
  if (dir.existsSync()) dir.deleteSync(recursive: true);
}

void main() {
  testWidgets('shows the About block with the app version and all new rows', (
    tester,
  ) async {
    final persistence = await _pumpSettings(tester);

    expect(
      find.text(
        'Version ${UpdateCheckService.appVersion} '
        '(build ${UpdateCheckService.appBuildNumber})',
      ),
      findsOneWidget,
    );
    expect(find.text('Check for updates'), findsOneWidget);
    expect(find.text('Licenses'), findsOneWidget);
    expect(find.text('Repair library'), findsOneWidget);
    expect(find.text('Export diagnostics…'), findsOneWidget);

    await _teardown(tester, persistence);
  });

  testWidgets('offers the input-channel choice for a 2-channel input only', (
    tester,
  ) async {
    var channels = 2;
    final settings = AppSettingsService(
      storage: LocalStorageService(),
      enumerateOutputs: () => const [],
      enumerateInputs: () => const [],
      inputChannelCount: (_) => channels,
    );
    final persistence = await _pumpSettings(tester, settings: settings);
    await tester.pump(); // the screen's first device refresh lands

    expect(find.text('Input channel'), findsOneWidget);
    expect(find.text('Auto'), findsOneWidget);
    expect(settings.inputChannel, GraphAudioPlaybackService.inputChannelAuto);
    await tester.tap(find.text('Input 2'));
    await tester.pump();
    expect(settings.inputChannel, GraphAudioPlaybackService.inputChannel2);

    // A mono input has nothing to choose between.
    channels = 1;
    settings.refreshDevices();
    await tester.pump();
    expect(find.text('Input channel'), findsNothing);

    await _teardown(tester, persistence);
  });

  testWidgets('names the device the input-channel choice applies to', (
    tester,
  ) async {
    // The chosen interface is unplugged, so the default records — and a
    // choice made under the chosen one's offline row lands on the default.
    final settings = AppSettingsService(
      storage: LocalStorageService(),
      enumerateOutputs: () => const [],
      enumerateInputs: () => const [
        AudioDevice(
          uid: 'second',
          name: 'Second Interface',
          isDefault: true,
          isInput: true,
        ),
      ],
      inputChannelCount: (_) => 2,
    );
    // Real file I/O: it never completes inside the test's fake async zone.
    await tester.runAsync(() => settings.setInputDevice('unplugged'));
    final persistence = await _pumpSettings(tester, settings: settings);
    await tester.pump();

    expect(
      find.textContaining('Second Interface — Auto follows'),
      findsOneWidget,
    );

    await _teardown(tester, persistence);
  });

  testWidgets('manual update check surfaces a newer release and its dialog', (
    tester,
  ) async {
    final updates = UpdateCheckService(
      fetch: (_) async =>
          '{"latest": "9.9.9", "url": "https://example.com/dl", '
          '"notes": "Better vibrato analysis."}',
      currentVersion: '1.0.0',
    );
    final persistence = await _pumpSettings(tester, updates: updates);

    expect(
      find.text('Compares this build against the latest release.'),
      findsOneWidget,
    );
    await tester.tap(find.text('Check for updates'));
    await tester.pumpAndSettle();

    // The download dialog opened; behind it the row now shows the result.
    expect(find.text('Vocara 9.9.9 is available'), findsOneWidget);
    expect(find.text('Better vibrato analysis.'), findsOneWidget);
    await tester.tap(find.text('Later'));
    await tester.pumpAndSettle();
    expect(
      find.text('Vocara 9.9.9 is available. Click for the download.'),
      findsOneWidget,
    );

    await _teardown(tester, persistence);
  });

  testWidgets('manual update check reports up to date without a dialog', (
    tester,
  ) async {
    final persistence = await _pumpSettings(tester);

    await tester.tap(find.text('Check for updates'));
    await tester.pumpAndSettle();
    expect(find.text('You have the latest version.'), findsOneWidget);
    expect(find.text('Later'), findsNothing);

    await _teardown(tester, persistence);
  });

  testWidgets('a failed manual check degrades to a gentle retry line', (
    tester,
  ) async {
    final updates = UpdateCheckService(
      fetch: (_) async => throw Exception('offline'),
      currentVersion: '1.0.0',
    );
    final persistence = await _pumpSettings(tester, updates: updates);

    await tester.tap(find.text('Check for updates'));
    await tester.pumpAndSettle();
    expect(
      find.text('Could not reach the update server. You can try again later.'),
      findsOneWidget,
    );

    await _teardown(tester, persistence);
  });

  testWidgets('Repair library confirms, runs repairLibrary and toasts the '
      'report', (tester) async {
    final persistence = await _pumpSettings(tester);

    await tester.tap(find.text('Repair library'));
    await tester.pumpAndSettle();
    // Plain-language confirm dialog.
    expect(find.text('Repair library?'), findsOneWidget);
    expect(
      find.textContaining('Your practices and recordings are not touched.'),
      findsOneWidget,
    );

    await tester.tap(find.text('Repair'));
    await tester.pump();
    // A clean in-memory store: nothing found, nothing swept.
    await _pumpUntilFound(
      tester,
      find.text('Repair finished. Everything looks consistent.'),
    );

    await _teardown(tester, persistence);
  });

  group('repairSummary', () {
    test('a damaged file is not reported as an all-clear', () {
      // A clean database can still hold a bit-rotted pool file: the integrity
      // check and verifyPoolIntegrity are independent, so this triple is
      // reachable — and used to read "Everything looks consistent. 1 audio
      // file looks damaged", which contradicts itself.
      final s = repairSummary(
        const RepairReport(
          violationsFound: 0,
          violationsRepaired: 0,
          assetsSwept: 0,
          corruptPoolFiles: 1,
        ),
      );
      expect(s, isNot(contains('Everything looks consistent')));
      expect(s, contains('Nothing needed fixing.'));
      expect(s, contains('1 audio file looks damaged'));
    });

    test('the all-clear survives when nothing is damaged', () {
      expect(
        repairSummary(
          const RepairReport(
            violationsFound: 0,
            violationsRepaired: 0,
            assetsSwept: 0,
            corruptPoolFiles: 0,
          ),
        ),
        'Repair finished. Everything looks consistent.',
      );
    });

    test('more than one damaged file reads as plural', () {
      expect(
        repairSummary(
          const RepairReport(
            violationsFound: 0,
            violationsRepaired: 0,
            assetsSwept: 0,
            corruptPoolFiles: 2,
          ),
        ),
        contains('2 audio files look damaged'),
      );
    });
  });

  testWidgets('Repair library cancel runs nothing', (tester) async {
    final persistence = await _pumpSettings(tester);

    await tester.tap(find.text('Repair library'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Cancel'));
    await tester.pumpAndSettle();
    expect(find.textContaining('Repair finished'), findsNothing);

    await _teardown(tester, persistence);
  });

  testWidgets('Export diagnostics writes the bundle to the picked path', (
    tester,
  ) async {
    final dir = Directory.systemTemp.createTempSync('vocara_diag_test');
    addTearDown(() => dir.deleteSync(recursive: true));
    final destination = p.join(dir.path, 'vocara-diagnostics.txt');

    // A real DiagnosticsLog owned by THIS test: the shared singleton's write
    // queue is a Future born in whichever earlier test touched it first, and
    // awaiting a future across flutter_test's per-test fake-async zones never
    // resumes. Production has one zone, so only tests need this seam.
    final log = DiagnosticsLog.forTesting(
      directory: Directory(p.join(dir.path, 'log'))..createSync(),
    )..log('settings-test', 'one entry for the bundle');

    final persistence = await _pumpSettings(
      tester,
      pickSavePath: (suggestedName) async {
        expect(suggestedName, 'vocara-diagnostics.txt');
        return destination;
      },
      exportDiagnostics: log.exportArchive,
    );

    await tester.tap(find.text('Export diagnostics…'));
    await tester.pump();
    await _pumpUntilFound(tester, find.textContaining('Diagnostics saved'));

    final exported = File(destination);
    expect(exported.existsSync(), isTrue);
    expect(exported.readAsStringSync(), contains('Vocara diagnostics bundle'));

    await _teardown(tester, persistence);
  });

  testWidgets('Export diagnostics does nothing when the save panel is '
      'cancelled', (tester) async {
    final persistence = await _pumpSettings(
      tester,
      pickSavePath: (_) async => null,
    );

    await tester.tap(find.text('Export diagnostics…'));
    await tester.pumpAndSettle();
    expect(find.textContaining('Diagnostics saved'), findsNothing);

    await _teardown(tester, persistence);
  });

  testWidgets('Licenses opens the license page with the hand-maintained '
      'bundled-component entries registered', (tester) async {
    final persistence = await _pumpSettings(tester);

    await tester.tap(find.text('Licenses'));
    await tester.pump();
    await tester.pump();
    expect(find.byType(LicensePage), findsOneWidget);

    // The registry now carries the non-pub components that ship in the app
    // bundle (spot checks; licenses.dart holds the full list).
    final entries = await tester.runAsync(
      () => LicenseRegistry.licenses.toList(),
    );
    final packages = <String>{for (final e in entries!) ...e.packages};
    expect(packages, contains('mpv (libmpv)'));
    expect(packages, contains('FFmpeg'));
    expect(packages, contains('dav1d'));
    expect(packages, contains('SQLite'));
    expect(packages, contains('cpal (Rust crate)'));
    expect(packages, contains('Symphonia (Rust crate)'));

    // Registration is idempotent across openings.
    registerBundledComponentLicenses();
    final again = await tester.runAsync(
      () => LicenseRegistry.licenses.toList(),
    );
    expect(again!.length, entries.length);

    await _teardown(tester, persistence);
  });
}
