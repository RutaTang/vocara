// A practice delete confirmed while another one still runs: the command drops
// a call made while it runs, so the second card stayed, with no word said.

import 'dart:async';

import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:forui/forui.dart';
import 'package:provider/provider.dart';
import 'package:vocara/data/repositories/song_practice_repository.dart';
import 'package:vocara/data/services/update_check_service.dart';
import 'package:vocara/domain/models/practice_summary.dart';
import 'package:vocara/ui/core/widgets/studio_button.dart';
import 'package:vocara/ui/home/view_model/home_view_model.dart';
import 'package:vocara/ui/home/widgets/home_screen.dart';
import 'package:vocara/utils/result.dart';

/// Two practices; the first delete parks on [gate].
class _Repo extends Fake implements SongPracticeRepository {
  final summaries = [
    for (final (id, title) in [('1', 'First'), ('2', 'Second')])
      PracticeSummary(
        id: id,
        title: title,
        updatedAt: DateTime(2026),
        takeCount: 0,
        hasAccompaniment: false,
        noteCount: 0,
      ),
  ];
  final gate = Completer<void>();
  var deletes = 0;

  @override
  Future<Result<List<PracticeSummary>>> listPracticeSummaries() async =>
      Result.ok([...summaries]);

  @override
  Future<Result<void>> deletePractice(String id) async {
    if (deletes++ == 0) await gate.future;
    summaries.removeWhere((s) => s.id == id);
    return const Result.ok(null);
  }
}

void main() {
  testWidgets('a delete confirmed while another runs waits its turn instead '
      'of being dropped', (tester) async {
    tester.view.physicalSize = const Size(2560, 1440);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    final repo = _Repo();
    await tester.pumpWidget(
      MultiProvider(
        providers: [
          ChangeNotifierProvider(
            create: (_) => UpdateCheckService(fetch: (_) async => '{}'),
          ),
        ],
        child: MaterialApp(
          home: FTheme(
            data: FThemes.zinc.dark,
            child: ChangeNotifierProvider(
              create: (_) => HomeViewModel(repository: repo),
              child: const HomeScreen(),
            ),
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();
    final mouse = await tester.createGesture(kind: PointerDeviceKind.mouse);
    addTearDown(mouse.removePointer);

    Future<void> deleteCard(String title) async {
      await mouse.moveTo(tester.getCenter(find.text(title)));
      await tester.pump();
      await tester.tap(find.byIcon(FIcons.trash2));
      await tester.pumpAndSettle();
      await tester.tap(find.widgetWithText(StudioButton, 'Delete').last);
      await tester.pump();
    }

    await deleteCard('First'); // parks in the repository
    await deleteCard('Second');
    repo.gate.complete();
    await tester.pumpAndSettle();

    expect(repo.summaries, isEmpty);
    expect(find.text('Second'), findsNothing);
  });
}
