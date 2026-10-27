import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:forui/forui.dart';
import 'package:integration_test/integration_test.dart';
import 'package:media_kit/media_kit.dart';
import 'package:vocara/data/db/persistence.dart';
import 'package:vocara/data/services/source_separation_service.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/ui/core/widgets/studio_button.dart';
import 'package:vocara/ui/song_practice_detail/widgets/bottom_dock.dart';
import 'package:vocara/ui/song_practice_detail/widgets/song_practice_detail_screen.dart';
import 'package:vocara/ui/song_practice_detail/widgets/track_row.dart';
import 'package:vocara/utils/result.dart';

import 'support/e2e.dart';

/// UI-driven journeys that deep-link straight into a seeded practice's detail
/// screen and drive the arrange surface with taps and a simulated OS file drop:
/// transport, take expansion, the analysis dock, backing import, and the
/// interpretation switcher.
///
/// MUST be run with the matching START deep-link (the seed id the suite inserts
/// in setUp):
///   flutter test integration_test/detail_arrange_journey_test.dart -d macos \
///       --dart-define=START=/song/E2EPRACTICESEED00000000000
///
/// Without that define the app boots Home and `bootDetail` fails fast with a
/// clear reason rather than hanging. The detail screen carries indefinite
/// painters (playhead, waveform, spectrogram), so we NEVER `pumpAndSettle` —
/// every wait is a bounded [pumpUntil] asserted `isTrue`.
void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  MediaKit.ensureInitialized();

  late Directory tmpDir;
  late Persistence persistence;

  setUp(() {
    tmpDir = Directory.systemTemp.createTempSync('vocara_detail_e2e');
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

  /// Seeds a practice with two real-WAV vocal takes under the fixed seed id,
  /// pumps the real tree, and waits until the deep-link has booted into the
  /// loaded detail screen with its takes rendered.
  Future<void> bootDetail(WidgetTester tester) async {
    await seedPracticeWithTakes(persistence.songRepo, tmpDir, takeCount: 2);
    await pumpApp(
      tester,
      persistence,
      separation: FakeSourceSeparationService(),
    );

    expect(
      await pumpUntil(
        tester,
        () => present(find.byType(SongPracticeDetailScreen)),
      ),
      isTrue,
      reason:
          'should deep-link into the detail screen — run with '
          '--dart-define=START=/song/$kSeedPracticeId',
    );
    expect(
      await pumpUntil(tester, () => present(find.text('E2E Practice'))),
      isTrue,
      reason:
          'the seeded practice loads (its title shows in the transport bar)',
    );
    expect(
      await pumpUntil(tester, () => present(find.byType(TrackRow))),
      isTrue,
      reason: 'the seeded vocal takes render as track rows',
    );
  }

  testWidgets('deep-link boots into the seeded practice with both takes', (
    tester,
  ) async {
    await bootDetail(tester);

    // No backing or FX buses are seeded, and the master strip is not a
    // TrackRow — so the only track rows are the two vocal takes. Both rows
    // present proves both takes loaded from the deep-linked practice.
    expect(
      find.byType(TrackRow),
      findsNWidgets(2),
      reason: 'both seeded vocal takes render as track rows',
    );
    expect(
      find.descendant(
        of: find.byType(SongPracticeDetailScreen),
        matching: find.byIcon(FIcons.play),
      ),
      findsOneWidget,
      reason: 'transport play is available once the practice has tracks',
    );
  });

  testWidgets('the transport toggles play ↔ pause', (tester) async {
    await bootDetail(tester);

    final play = find.descendant(
      of: find.byType(SongPracticeDetailScreen),
      matching: find.byIcon(FIcons.play),
    );
    expect(play, findsOneWidget, reason: 'starts paused, showing a play icon');

    await tester.tap(play);
    expect(
      await pumpUntil(tester, () => present(find.byIcon(FIcons.pause))),
      isTrue,
      reason: 'tapping play starts playback and swaps the icon to pause',
    );

    await tester.tap(find.byIcon(FIcons.pause));
    expect(
      await pumpUntil(tester, () => present(find.byIcon(FIcons.play))),
      isTrue,
      reason: 'tapping pause stops playback and swaps the icon back to play',
    );
  });

  testWidgets('the stop button halts playback and resets the icon to play', (
    tester,
  ) async {
    await bootDetail(tester);

    await tester.tap(
      find.descendant(
        of: find.byType(SongPracticeDetailScreen),
        matching: find.byIcon(FIcons.play),
      ),
    );
    expect(
      await pumpUntil(tester, () => present(find.byIcon(FIcons.pause))),
      isTrue,
      reason: 'playback starts (pause icon shows)',
    );

    // Stop (the square) pauses and rewinds the playhead to the loop start, so
    // the transport returns to the idle play icon.
    await tester.tap(find.byIcon(FIcons.square));
    expect(
      await pumpUntil(tester, () => present(find.byIcon(FIcons.play))),
      isTrue,
      reason: 'stop halts playback and the icon returns to play',
    );
  });

  testWidgets(
    'expanding a take flips its disclosure chevron and reveals lanes',
    (tester) async {
      await bootDetail(tester);

      final firstTrack = find.byType(TrackRow).first;
      final expand = find.descendant(
        of: firstTrack,
        matching: find.byIcon(FIcons.chevronRight),
      );
      expect(
        expand,
        findsOneWidget,
        reason: 'a collapsed take shows an expand chevron',
      );

      await tester.tap(expand);
      expect(
        await pumpUntil(
          tester,
          () => present(
            find.descendant(
              of: find.byType(TrackRow).first,
              matching: find.byIcon(FIcons.chevronDown),
            ),
          ),
        ),
        isTrue,
        reason: 'expanding flips the chevron and renders the sub-lanes inline',
      );
    },
  );

  testWidgets('the bottom analysis dock opens', (tester) async {
    await bootDetail(tester);

    expect(find.byType(BottomDock), findsWidgets);
    final open = find.byIcon(FIcons.panelBottomOpen);
    expect(open, findsOneWidget, reason: 'the dock starts closed');

    await tester.tap(open);
    expect(
      await pumpUntil(
        tester,
        () => present(find.byIcon(FIcons.panelBottomClose)),
      ),
      isTrue,
      reason: 'tapping the toggle opens the dock (close icon now shows)',
    );
  });

  testWidgets('dropping an audio file imports the backing track', (
    tester,
  ) async {
    await bootDetail(tester);

    // textContaining, not text: the zone's copy gained ", or click to browse"
    // when click-to-upload landed, and an exact-match finder simply stopped
    // matching — the assertion below then failed for a reason that had nothing
    // to do with dropping a file.
    final zone = find.textContaining('Drag a backing track here');
    expect(
      await pumpUntil(tester, () => present(zone)),
      isTrue,
      reason: 'with no accompaniment seeded, the backing drop zone shows',
    );

    final wav = await writeSineWav(
      '${tmpDir.path}/backing_drop.wav',
      freq: 110,
    );
    await simulateFileDrop(tester, [wav], at: tester.getCenter(zone));

    expect(
      await pumpUntil(tester, () => !present(zone)),
      isTrue,
      reason:
          'a dropped audio file is imported as the backing track, replacing '
          'the drop zone',
    );
  });

  testWidgets('the interpretation switcher opens its menu', (tester) async {
    await bootDetail(tester);

    final switcher = find.byIcon(FIcons.drama);
    expect(switcher, findsOneWidget, reason: 'the active-interpretation chip');

    await tester.tap(switcher);
    expect(
      await pumpUntil(tester, () => present(find.text('New interpretation'))),
      isTrue,
      reason: 'tapping the switcher opens the interpretation menu',
    );
    expect(
      find.text('Interpretation 1'),
      findsWidgets,
      reason: 'the seeded interpretation is listed in the menu',
    );
  });

  testWidgets('the "New take" button adds an empty vocal lane', (tester) async {
    await bootDetail(tester);
    expect(find.byType(TrackRow), findsNWidgets(2));

    await tester.tap(find.text('New take'));
    expect(
      await pumpUntil(
        tester,
        () => find.byType(TrackRow).evaluate().length == 3,
      ),
      isTrue,
      reason: 'New take adds a third (empty) vocal lane',
    );
  });

  testWidgets('adding a note via the take header persists it on the take', (
    tester,
  ) async {
    await bootDetail(tester);

    // The always-visible sticky-note button on the first take opens the editor.
    final noteButton = find.descendant(
      of: find.byType(TrackRow).first,
      matching: find.byIcon(FIcons.stickyNote),
    );
    expect(noteButton, findsOneWidget);

    await tester.tap(noteButton);
    expect(
      await pumpUntil(
        tester,
        () => present(find.widgetWithText(StudioButton, 'Add')),
      ),
      isTrue,
      reason: 'the sticky-note button opens the note editor',
    );

    await tester.enterText(find.byType(TextField), 'breath runs out here');
    // Let the text land in a frame before confirming, so the tap meets the
    // editor as it stands with the note typed.
    await tester.pump();
    await tester.tap(find.widgetWithText(StudioButton, 'Add'));
    expect(
      await pumpUntil(
        tester,
        () => !present(find.widgetWithText(StudioButton, 'Add')),
      ),
      isTrue,
      reason: 'confirming the editor closes it',
    );

    // The note round-trips to the store on one of the vocal takes.
    final reloaded =
        (await persistence.songRepo.getPractice(kSeedPracticeId)
                as Ok<SongPractice>)
            .value;
    final notes = reloaded.vocalTracks
        .expand((t) => t.notes)
        .map((n) => n.content)
        .toList();
    expect(
      notes,
      contains('breath runs out here'),
      reason: 'the note persisted through the UI-driven add',
    );
  });
}
