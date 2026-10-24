// Home's update-check wiring: the once-per-run auto-check fires ~5 s after
// home mounts (never on mount, never twice), and a newer release raises the
// dismissible banner whose Download button opens the release dialog with a
// selectable URL. All network is replaced by an injected fetcher.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:provider/provider.dart';
import 'package:vocara/data/repositories/song_practice_repository.dart';
import 'package:vocara/data/services/update_check_service.dart';
import 'package:vocara/data/sync/sync_hashes.dart';
import 'package:vocara/domain/models/practice_summary.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/ui/home/view_model/home_view_model.dart';
import 'package:vocara/ui/home/widgets/home_screen.dart';
import 'package:vocara/ui/home/widgets/update_notice.dart';
import 'package:vocara/utils/result.dart';

class _FakeRepo implements SongPracticeRepository {
  @override
  Future<Result<List<SongPractice>>> listPractices() async =>
      const Result.ok([]);

  @override
  Future<Result<List<PracticeSummary>>> listPracticeSummaries() async =>
      const Result.ok([]);

  @override
  Future<Result<SongPractice>> getPractice(String id) async =>
      Result.error(Exception('not found'));

  @override
  Future<Result<Loaded<SongPractice>>> loadPractice(String id) async =>
      Result.error(Exception('not found'));

  @override
  Future<Result<SongPractice>> createPractice(String title) async =>
      Result.error(Exception('unsupported'));

  @override
  Future<Result<SongPractice>> savePractice(SongPractice practice) async =>
      Result.ok(practice);

  @override
  Future<Result<Loaded<SongPractice>>> savePracticeWithBase(
    SongPractice practice, {
    required Map<String, String> baseHashes,
  }) async => Result.ok(Loaded(practice, const {}));

  @override
  Future<Result<void>> deletePractice(String id) async => const Result.ok(null);
}

Future<UpdateCheckService> _pumpHome(
  WidgetTester tester, {
  required UpdateCheckService updates,
}) async {
  final vm = HomeViewModel(repository: _FakeRepo());
  await tester.pumpWidget(
    MultiProvider(
      providers: [
        ChangeNotifierProvider<HomeViewModel>.value(value: vm),
        ChangeNotifierProvider<UpdateCheckService>.value(value: updates),
      ],
      child: const MaterialApp(home: HomeScreen()),
    ),
  );
  await tester.pump();
  return updates;
}

UpdateCheckService _newerAvailable({required List<int> fetchCounter}) {
  return UpdateCheckService(
    fetch: (_) async {
      fetchCounter.add(1);
      return '{"latest": "9.9.9", "url": "https://example.com/download", '
          '"notes": "New voiceprint views."}';
    },
    currentVersion: '1.0.0',
  );
}

void main() {
  testWidgets('auto-check fires once ~5s after mount and raises the banner', (
    tester,
  ) async {
    final fetches = <int>[];
    await _pumpHome(tester, updates: _newerAvailable(fetchCounter: fetches));

    // Nothing before the deferred check fires.
    await tester.pump(const Duration(seconds: 4));
    expect(fetches, isEmpty);
    expect(find.textContaining('9.9.9'), findsNothing);

    // At ~5s the check runs and the banner appears.
    await tester.pump(const Duration(seconds: 2));
    await tester.pump();
    expect(fetches, hasLength(1));
    expect(find.text('Vocara 9.9.9 is available.'), findsOneWidget);
    expect(find.text('Download'), findsOneWidget);

    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('up-to-date auto-check shows no banner and no error', (
    tester,
  ) async {
    final updates = UpdateCheckService(
      fetch: (_) async => '{"latest": "1.0.0", "url": "https://example.com"}',
      currentVersion: '1.0.0',
    );
    await _pumpHome(tester, updates: updates);
    await tester.pump(const Duration(seconds: 6));
    await tester.pump();
    expect(updates.status, UpdateStatus.upToDate);
    expect(find.text('Download'), findsNothing);
    // The silent policy: nothing error-shaped ever shows on an auto-check.
    expect(find.byType(SnackBar), findsNothing);

    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('a failed auto-check stays silent', (tester) async {
    final updates = UpdateCheckService(
      fetch: (_) async => throw Exception('offline'),
      currentVersion: '1.0.0',
    );
    await _pumpHome(tester, updates: updates);
    await tester.pump(const Duration(seconds: 6));
    await tester.pump();
    expect(updates.status, UpdateStatus.unknown);
    expect(find.text('Download'), findsNothing);
    expect(find.byType(SnackBar), findsNothing);

    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('Download opens the dialog with notes and a selectable URL', (
    tester,
  ) async {
    final updates = _newerAvailable(fetchCounter: <int>[]);
    await _pumpHome(tester, updates: updates);
    await tester.pump(const Duration(seconds: 6));
    await tester.pump();

    await tester.tap(find.text('Download'));
    await tester.pumpAndSettle();
    expect(find.text('Vocara 9.9.9 is available'), findsOneWidget);
    expect(find.text('New voiceprint views.'), findsOneWidget);
    expect(
      find.byWidgetPredicate(
        (w) => w is SelectableText && w.data == 'https://example.com/download',
      ),
      findsOneWidget,
    );

    await tester.tap(find.text('Later'));
    await tester.pumpAndSettle();
    expect(find.text('Vocara 9.9.9 is available'), findsNothing);

    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('dismissing the banner hides it for the rest of the run', (
    tester,
  ) async {
    final updates = _newerAvailable(fetchCounter: <int>[]);
    await _pumpHome(tester, updates: updates);
    await tester.pump(const Duration(seconds: 6));
    await tester.pump();
    expect(find.text('Vocara 9.9.9 is available.'), findsOneWidget);

    await tester.tap(find.byTooltip('Dismiss'));
    await tester.pump();
    expect(find.text('Vocara 9.9.9 is available.'), findsNothing);
    expect(updates.noticeDismissed, isTrue);

    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('the injected openUrl is used by the dialog button', (
    tester,
  ) async {
    final updates = _newerAvailable(fetchCounter: <int>[]);
    await updates.check();
    final opened = <String>[];
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: UpdateNoticeBanner(
            updates: updates,
            openUrl: (url) async {
              opened.add(url);
              return true;
            },
          ),
        ),
      ),
    );
    await tester.tap(find.text('Download'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Open Download Page'));
    await tester.pump();
    expect(opened, ['https://example.com/download']);

    await tester.pumpWidget(const SizedBox());
  });
}
