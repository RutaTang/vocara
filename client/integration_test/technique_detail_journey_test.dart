import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:media_kit/media_kit.dart';
import 'package:vocara/data/db/persistence.dart';
import 'package:vocara/data/services/source_separation_service.dart';
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/ui/core/widgets/studio_button.dart';
import 'package:vocara/ui/technique_library/widgets/studio_field.dart';
import 'package:vocara/ui/technique_library/widgets/technique_detail_screen.dart';
import 'package:vocara/utils/result.dart';

import 'support/e2e.dart';

/// UI-driven journeys that deep-link into a seeded technique's detail screen and
/// drive the editor with real taps, text entry, and a simulated OS file drop —
/// then verify the outcome by reloading the entity through the SAME drift repo
/// the test owns (so each journey asserts a real UI-drive → persistence
/// round-trip, not just a transient widget state).
///
/// MUST be run with the matching START deep-link:
///   flutter test integration_test/technique_detail_journey_test.dart -d macos \
///       --dart-define=START=/techniques/E2ETECHNIQUESEED000000000001
///
/// Without that define the app boots Home and `bootTechnique` fails fast. The
/// editor's analysis panels carry indefinite painters, so we NEVER
/// `pumpAndSettle` — every wait is a bounded [pumpUntil] asserted `isTrue`.
void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  // A saved/added audio example builds a media_kit-backed player + analysis.
  MediaKit.ensureInitialized();

  late Directory tmpDir;
  late Persistence persistence;

  setUp(() {
    tmpDir = Directory.systemTemp.createTempSync('vocara_technique_e2e');
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

  Future<void> bootTechnique(WidgetTester tester) async {
    await seedTechnique(persistence.techRepo, name: 'E2E Technique');
    await pumpApp(tester, persistence, separation: FakeSourceSeparationService());

    expect(
      await pumpUntil(
        tester,
        () => present(find.byType(TechniqueDetailScreen)),
      ),
      isTrue,
      reason:
          'should deep-link into technique detail — run with '
          '--dart-define=START=/techniques/$kSeedTechniqueId',
    );
    expect(
      await pumpUntil(tester, () => present(find.text('E2E Technique'))),
      isTrue,
      reason: 'the seeded technique loads (its name shows in the header)',
    );
  }

  /// The borderless [TextField] inside the edit-form [StudioField] labelled
  /// [label] (e.g. 'Name', 'Summary').
  Finder editField(String label) => find.descendant(
        of: find.byWidgetPredicate((w) => w is StudioField && w.label == label),
        matching: find.byType(TextField),
      );

  Future<Technique> reload() async =>
      (await persistence.techRepo.getTechnique(kSeedTechniqueId) as Ok<Technique>)
          .value;

  testWidgets('Edit → change fields → Save persists through the repo', (
    tester,
  ) async {
    await bootTechnique(tester);

    await tester.tap(find.widgetWithText(StudioButton, 'Edit'));
    expect(
      await pumpUntil(tester, () => present(find.text('Edit technique'))),
      isTrue,
      reason: 'tapping Edit opens the editor form',
    );

    expect(editField('Name'), findsOneWidget);
    await tester.enterText(editField('Name'), 'Belted Twang');
    expect(editField('Summary'), findsOneWidget);
    await tester.enterText(editField('Summary'), 'bright, ringing edge');
    await tester.pump();

    await tester.tap(find.widgetWithText(StudioButton, 'Save'));
    expect(
      await pumpUntil(
        tester,
        () => present(find.widgetWithText(StudioButton, 'Edit')),
      ),
      isTrue,
      reason: 'a successful save returns to view mode (Edit reappears)',
    );

    final reloaded = await reload();
    expect(reloaded.name, 'Belted Twang');
    expect(reloaded.summary, 'bright, ringing edge');
  });

  testWidgets('Edit → drop a WAV example → Save persists the media', (
    tester,
  ) async {
    await bootTechnique(tester);

    await tester.tap(find.widgetWithText(StudioButton, 'Edit'));
    expect(
      await pumpUntil(tester, () => present(find.text('Edit technique'))),
      isTrue,
    );

    // With no media yet, the add zone shows its full prompt.
    const addZone = 'Drag an audio or video clip here, or click to browse';
    expect(
      await pumpUntil(tester, () => present(find.text(addZone))),
      isTrue,
      reason: 'edit mode exposes the media add/drop zone',
    );

    final wav = await writeSineWav('${tmpDir.path}/example.wav', freq: 330);
    await simulateFileDrop(
      tester,
      [wav],
      at: tester.getCenter(find.text(addZone)),
    );

    // Once imported, the add zone goes dense ('Add another example') because
    // effectiveMedia is non-empty — a UI proxy that the import finished.
    expect(
      await pumpUntil(tester, () => present(find.text('Add another example'))),
      isTrue,
      reason: 'the dropped clip is imported and staged in the draft',
    );

    await tester.tap(find.widgetWithText(StudioButton, 'Save'));
    expect(
      await pumpUntil(
        tester,
        () => present(find.widgetWithText(StudioButton, 'Edit')),
      ),
      isTrue,
      reason: 'save returns to view mode',
    );

    final reloaded = await reload();
    expect(
      reloaded.media,
      hasLength(1),
      reason: 'the staged example persisted on save',
    );
    expect(File(reloaded.media.single.path).existsSync(), isTrue,
        reason: 'the pooled example file resolves on disk');
  });

  testWidgets('Edit → change a field → Cancel discards via the confirm dialog',
      (tester) async {
    await bootTechnique(tester);

    await tester.tap(find.widgetWithText(StudioButton, 'Edit'));
    expect(
      await pumpUntil(tester, () => present(find.text('Edit technique'))),
      isTrue,
    );

    await tester.enterText(editField('Name'), 'Should Not Persist');
    await tester.pump();

    await tester.tap(find.widgetWithText(StudioButton, 'Cancel'));
    expect(
      await pumpUntil(
        tester,
        () => present(find.text('Discard unsaved changes?')),
      ),
      isTrue,
      reason: 'cancelling a dirty editor asks to confirm first',
    );

    await tester.tap(find.widgetWithText(StudioButton, 'Discard'));
    expect(
      await pumpUntil(
        tester,
        () =>
            present(find.widgetWithText(StudioButton, 'Edit')) &&
            !present(find.text('Discard unsaved changes?')),
      ),
      isTrue,
      reason: 'discarding returns to view mode',
    );

    final reloaded = await reload();
    expect(
      reloaded.name,
      'E2E Technique',
      reason: 'a discarded edit never reaches the store',
    );
  });
}
