// A save started on a practice's screen can outlive it: a stopped take lands
// after Back. Home reloaded its list as the screen popped and never again, so
// the card kept the practice as it was before the save.

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:go_router/go_router.dart';
import 'package:provider/provider.dart';
import 'package:vocara/data/db/persistence.dart';
import 'package:vocara/data/repositories/song_practice_repository.dart';
import 'package:vocara/data/repositories/technique_repository.dart';
import 'package:vocara/data/services/recording_finalizer.dart';
import 'package:vocara/data/services/update_check_service.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/routing/routes.dart';
import 'package:vocara/ui/home/view_model/home_view_model.dart';
import 'package:vocara/ui/home/widgets/home_screen.dart';
import 'package:vocara/ui/home/widgets/practice_project_card.dart';
import 'package:vocara/utils/result.dart';

/// Stands in for the practice screen: starts a save that lands later, then
/// goes Back.
class _LeavesASaveLanding extends StatefulWidget {
  const _LeavesASaveLanding({required this.id, required this.save});

  final String id;
  final Future<void> Function() save;

  @override
  State<_LeavesASaveLanding> createState() => _LeavesASaveLandingState();
}

class _LeavesASaveLandingState extends State<_LeavesASaveLanding> {
  @override
  void initState() {
    super.initState();
    unawaited(RecordingFinalizer.instance.landing(widget.id, widget.save));
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) context.pop();
    });
  }

  @override
  Widget build(BuildContext context) => const SizedBox();
}

void main() {
  testWidgets('a save still landing after Back reaches the card once it '
      'lands', (tester) async {
    tester.view.physicalSize = const Size(2560, 1440);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);

    final persistence = Persistence.memory();
    late SongPractice practice;
    await tester.runAsync(() async {
      practice =
          (await persistence.songRepo.createPractice('First')
                  as Ok<SongPractice>)
              .value;
    });
    final landing = Completer<void>();
    final router = GoRouter(
      routes: [
        GoRoute(
          path: Routes.home,
          builder: (context, _) => ChangeNotifierProvider(
            create: (context) => HomeViewModel(
              repository: context.read<SongPracticeRepository>(),
            ),
            child: const HomeScreen(),
          ),
        ),
        GoRoute(
          path: Routes.songPracticePattern,
          builder: (context, state) => _LeavesASaveLanding(
            id: state.pathParameters['id']!,
            save: () async {
              await landing.future;
              await persistence.songRepo.savePractice(
                practice.copyWith(title: 'First, landed'),
              );
            },
          ),
        ),
      ],
    );
    await tester.pumpWidget(
      MultiProvider(
        providers: [
          Provider<SongPracticeRepository>.value(value: persistence.songRepo),
          Provider<TechniqueRepository>.value(value: persistence.techRepo),
          ChangeNotifierProvider(
            create: (_) => UpdateCheckService(fetch: (_) async => '{}'),
          ),
        ],
        child: MaterialApp.router(routerConfig: router),
      ),
    );
    Future<void> frames([int n = 25]) async {
      for (var i = 0; i < n; i++) {
        await tester.pump(const Duration(milliseconds: 100));
      }
    }

    await frames(3);
    await tester.tap(find.widgetWithText(PracticeProjectCard, 'First'));
    // The opening dialog holds for ~1.5 s before the push; the stand-in pops.
    await frames();
    await tester.runAsync(
      () => Future<void>.delayed(const Duration(milliseconds: 50)),
    );
    await frames(5);
    expect(find.byType(HomeScreen), findsOneWidget);
    expect(find.widgetWithText(PracticeProjectCard, 'First'), findsOneWidget);

    landing.complete();
    await tester.runAsync(
      () => Future<void>.delayed(const Duration(milliseconds: 50)),
    );
    await frames(5);
    expect(
      find.widgetWithText(PracticeProjectCard, 'First, landed'),
      findsOneWidget,
    );

    await tester.pumpWidget(const SizedBox());
    await tester.runAsync(() => persistence.db.close());
  });
}
