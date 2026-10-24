// A page opened by a deep link (`--dart-define=START=…`, what the e2e suites
// boot into) is the ROOT of the navigator stack. Its back affordances used to
// call context.pop(), which go_router answers on a root page by throwing
// "There is nothing to pop": the chevron did nothing, and Delete left the user
// on the technique it had just deleted. With nothing beneath, they now go to
// the page's parent instead.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:forui/forui.dart';
import 'package:go_router/go_router.dart';
import 'package:provider/provider.dart';
import 'package:vocara/data/db/persistence.dart';
import 'package:vocara/data/repositories/technique_repository.dart';
import 'package:vocara/data/services/app_settings_service.dart';
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/data/services/technique_media_service.dart';
import 'package:vocara/data/services/update_check_service.dart';
import 'package:vocara/data/sync/sync_hashes.dart';
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/routing/routes.dart';
import 'package:vocara/ui/core/widgets/studio_button.dart';
import 'package:vocara/ui/settings/settings_screen.dart';
import 'package:vocara/ui/technique_library/technique_library_screen.dart';
import 'package:vocara/ui/technique_library/view_model/technique_detail_view_model.dart';
import 'package:vocara/ui/technique_library/view_model/technique_library_view_model.dart';
import 'package:vocara/ui/technique_library/widgets/technique_detail_screen.dart';
import 'package:vocara/utils/result.dart';

const _id = 'T1000000000000000000000000';

class _Repo implements TechniqueRepository {
  final techniques = <Technique>[
    Technique(
      id: _id,
      name: 'Twang',
      createdAt: DateTime(2026, 6, 20),
      updatedAt: DateTime(2026, 6, 20),
    ),
  ];

  @override
  Future<Result<List<Technique>>> listTechniques() async =>
      Result.ok(List.of(techniques));

  @override
  Future<Result<Technique>> getTechnique(String id) async =>
      Result.ok(techniques.firstWhere((t) => t.id == id));

  @override
  Future<Result<Loaded<Technique>>> loadTechnique(String id) async {
    final i = techniques.indexWhere((t) => t.id == id);
    if (i < 0) return Result.error(Exception('not found'));
    return Result.ok(Loaded(techniques[i], const {}));
  }

  @override
  Future<Result<Technique>> saveTechnique(Technique t) async => Result.ok(t);

  @override
  Future<Result<Loaded<Technique>>> saveTechniqueWithBase(
    Technique t, {
    required Map<String, String> baseHashes,
  }) async => Result.ok(Loaded(t, const {}));

  @override
  Future<Result<void>> deleteTechnique(String id) async {
    techniques.removeWhere((t) => t.id == id);
    return const Result.ok(null);
  }
}

/// Media service whose per-technique cleanup is a no-op (the real one
/// resolves the app-support dir through a platform channel).
class _NoDiskMedia extends TechniqueMediaService {
  _NoDiskMedia() : super(storage: LocalStorageService());

  @override
  Future<void> deleteTechniqueMedia(String techniqueId) async {}
}

/// Boots a router whose ONLY page is [start] (as a deep link does), with the
/// real technique screens and stand-ins for the pages they fall back to.
Future<_Repo> _boot(WidgetTester tester, String start) async {
  tester.view.physicalSize = const Size(1440, 1000);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  final repo = _Repo();
  final media = _NoDiskMedia();
  final router = GoRouter(
    initialLocation: start,
    routes: [
      GoRoute(path: Routes.home, builder: (_, _) => const Text('home page')),
      GoRoute(
        path: Routes.techniqueLibrary,
        builder: (_, _) => ChangeNotifierProvider(
          create: (_) =>
              TechniqueLibraryViewModel(repository: repo, mediaService: media),
          child: const TechniqueLibraryScreen(),
        ),
      ),
      GoRoute(
        path: Routes.techniqueDetailPattern,
        builder: (_, state) => ChangeNotifierProvider(
          create: (_) => TechniqueDetailViewModel(
            repository: repo,
            mediaService: media,
            techniqueId: state.pathParameters['id']!,
          ),
          child: const TechniqueDetailScreen(),
        ),
      ),
      GoRoute(path: Routes.settings, builder: (_, _) => const SettingsScreen()),
    ],
  );
  final persistence = Persistence.memory();
  addTearDown(() async {
    // Unmount first: Settings runs a device-poll timer.
    await tester.pumpWidget(const SizedBox());
    await tester.runAsync(() => persistence.db.close());
  });
  await tester.pumpWidget(
    MultiProvider(
      providers: [
        ChangeNotifierProvider(
          create: (_) => AppSettingsService(
            storage: LocalStorageService(),
            enumerateOutputs: () => const [],
            enumerateInputs: () => const [],
          ),
        ),
        Provider<Persistence>.value(value: persistence),
        ChangeNotifierProvider(
          create: (_) => UpdateCheckService(fetch: (_) async => '{}'),
        ),
      ],
      child: MaterialApp.router(routerConfig: router),
    ),
  );
  await tester.pump();
  await tester.pump();
  return repo;
}

Future<void> _settle(WidgetTester tester) async {
  for (var i = 0; i < 10; i++) {
    await tester.pump(const Duration(milliseconds: 100));
  }
}

void main() {
  testWidgets('a deep-linked technique: Delete lands on the library', (
    tester,
  ) async {
    final repo = await _boot(tester, Routes.techniqueDetail(_id));
    expect(find.byType(TechniqueDetailScreen), findsOneWidget);

    await tester.tap(find.widgetWithText(StudioButton, 'Delete'));
    await _settle(tester);
    await tester.tap(find.widgetWithText(StudioButton, 'Delete').last);
    await _settle(tester);

    expect(repo.techniques, isEmpty, reason: 'the delete itself ran');
    expect(find.byType(TechniqueDetailScreen), findsNothing);
    expect(find.byType(TechniqueLibraryScreen), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  testWidgets('a deep-linked technique: the back chevron lands on the '
      'library', (tester) async {
    await _boot(tester, Routes.techniqueDetail(_id));

    await tester.tap(
      find.descendant(
        of: find.byType(TechniqueDetailScreen),
        matching: find.byIcon(FIcons.chevronLeft),
      ),
    );
    await _settle(tester);

    expect(find.byType(TechniqueLibraryScreen), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  testWidgets('the deep-linked library and settings go back Home', (
    tester,
  ) async {
    await _boot(tester, Routes.techniqueLibrary);
    await tester.tap(
      find.descendant(
        of: find.byType(TechniqueLibraryScreen),
        matching: find.byIcon(FIcons.chevronLeft),
      ),
    );
    await _settle(tester);
    expect(find.text('home page'), findsOneWidget);
    expect(tester.takeException(), isNull);

    await _boot(tester, Routes.settings);
    await tester.tap(
      find.descendant(
        of: find.byType(SettingsScreen),
        matching: find.byIcon(FIcons.chevronLeft),
      ),
    );
    await _settle(tester);
    expect(find.text('home page'), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  testWidgets('a pushed technique still pops back to the library it came '
      'from', (tester) async {
    await _boot(tester, Routes.techniqueLibrary);
    await tester.tap(find.text('Twang'));
    await _settle(tester);
    expect(find.byType(TechniqueDetailScreen), findsOneWidget);

    await tester.tap(
      find.descendant(
        of: find.byType(TechniqueDetailScreen),
        matching: find.byIcon(FIcons.chevronLeft),
      ),
    );
    await _settle(tester);
    expect(find.byType(TechniqueDetailScreen), findsNothing);
    expect(find.byType(TechniqueLibraryScreen), findsOneWidget);
    // Popped, not replaced: the library underneath is the same page, so a
    // second open is not swallowed by its open guard.
    await tester.tap(find.text('Twang'));
    await _settle(tester);
    expect(find.byType(TechniqueDetailScreen), findsOneWidget);
  });
}
