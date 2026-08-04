import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:forui/forui.dart';
import 'package:provider/provider.dart';
import 'package:vocara/data/repositories/technique_repository.dart';
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/data/services/technique_media_service.dart';
import 'package:vocara/data/sync/sync_hashes.dart';
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/ui/core/widgets/studio_button.dart';
import 'package:vocara/ui/technique_library/technique_library_screen.dart';
import 'package:vocara/ui/technique_library/view_model/technique_library_view_model.dart';
import 'package:vocara/utils/result.dart';

class _FakeRepo implements TechniqueRepository {
  _FakeRepo(this.techniques);
  List<Technique> techniques;

  @override
  Future<Result<List<Technique>>> listTechniques() async =>
      Result.ok(techniques);

  @override
  Future<Result<Technique>> getTechnique(String id) async =>
      Result.ok(techniques.firstWhere((t) => t.id == id));

  @override
  Future<Result<Loaded<Technique>>> loadTechnique(String id) async =>
      Result.ok(Loaded(techniques.firstWhere((t) => t.id == id), const {}));

  @override
  Future<Result<Technique>> saveTechnique(Technique t) async => Result.ok(t);

  @override
  Future<Result<Loaded<Technique>>> saveTechniqueWithBase(
    Technique t, {
    required Map<String, String> baseHashes,
  }) async => Result.ok(Loaded(t, const {}));

  @override
  Future<Result<void>> deleteTechnique(String id) async =>
      const Result.ok(null);
}

/// A repo whose first [listTechniques] fails, then succeeds — models a
/// transient read failure so the error state and its Retry can be exercised.
class _FailOnceRepo extends _FakeRepo {
  _FailOnceRepo(super.techniques);

  bool _failNext = true;

  @override
  Future<Result<List<Technique>>> listTechniques() async {
    if (_failNext) {
      _failNext = false;
      return Result.error(Exception('simulated read failure'));
    }
    return super.listTechniques();
  }
}

Technique _t(String id, String name, List<String> tags) {
  final now = DateTime(2026, 6, 20);
  return Technique(
    id: id,
    name: name,
    tags: tags,
    summary: 'A summary for $name that is reasonably long to fill the card.',
    createdAt: now,
    updatedAt: now,
  );
}

Future<void> _pump(
  WidgetTester tester,
  List<Technique> techniques, {
  TechniqueRepository? repository,
}) async {
  final vm = TechniqueLibraryViewModel(
    repository: repository ?? _FakeRepo(techniques),
    mediaService: TechniqueMediaService(storage: LocalStorageService()),
  );
  await tester.pumpWidget(
    MaterialApp(
      home: ChangeNotifierProvider.value(
        value: vm,
        child: const TechniqueLibraryScreen(),
      ),
    ),
  );
  await tester.pump();
  await tester.pump();
}

void main() {
  testWidgets('populated grid + filter bar + rebuilds: no overflow', (
    tester,
  ) async {
    // A large window like the real app, where cards are full 300px wide.
    tester.view.physicalSize = const Size(1440, 900);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);

    await _pump(tester, [
      _t('1', 'Twang', ['EVT']),
      _t('2', 'Curbing', ['CVT']),
      _t('3', 'Mixed voice', const []),
      for (var i = 4; i < 24; i++)
        _t('$i', 'Technique number $i with a long name', ['EVT']),
    ]);
    expect(tester.takeException(), isNull, reason: 'initial layout');

    // Filtering rebuilds the grid (the path the runtime crash stack showed).
    await tester.tap(find.text('EVT').first);
    await tester.pump();
    expect(tester.takeException(), isNull, reason: 'after tag filter');

    // Hovering a card triggers a setState rebuild of that card.
    final gesture = await tester.createGesture(kind: PointerDeviceKind.mouse);
    await gesture.addPointer(location: Offset.zero);
    addTearDown(gesture.removePointer);
    // 'Twang' carries the EVT tag, so it survives the filter.
    await gesture.moveTo(tester.getCenter(find.text('Twang')));
    await tester.pump();
    expect(tester.takeException(), isNull, reason: 'after hover');
  });

  testWidgets('empty library lays out without overflow', (tester) async {
    await _pump(tester, const []);
    expect(tester.takeException(), isNull);
  });

  testWidgets('load failure shows the error state, and Retry reloads', (
    tester,
  ) async {
    // Regression: a failed list read used to fall through to the empty state,
    // indistinguishable from a genuinely empty library and with no way to
    // retry.
    final techniques = [
      _t('1', 'Twang', const ['EVT']),
    ];
    await _pump(tester, techniques, repository: _FailOnceRepo(techniques));

    // The error state — never the empty-library pitch — with a Retry button.
    expect(find.text('Could not load your techniques'), findsOneWidget);
    expect(find.widgetWithText(StudioButton, 'Retry'), findsOneWidget);
    expect(find.text('Build your technique library'), findsNothing);

    // Retry re-runs the load; the repo now succeeds and the grid appears.
    await tester.tap(find.text('Retry'));
    await tester.pump();
    await tester.pump();
    expect(find.text('Twang'), findsOneWidget);
    expect(find.text('Could not load your techniques'), findsNothing);
    expect(tester.takeException(), isNull);
  });

  testWidgets('header cluster right-anchors; New technique hugs the edge', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(1440, 900);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);

    await _pump(tester, [
      _t('1', 'Twang', ['EVT']),
      _t('2', 'Curbing', ['CVT']),
    ]);
    // The tag filter is a compact dropdown button (default label "All tags").
    expect(find.text('All tags'), findsOneWidget);

    final screenWidth = tester.getSize(find.byType(MaterialApp)).width;
    // New technique sits flush against the right edge.
    final btnRight = tester
        .getBottomRight(find.widgetWithText(StudioButton, 'New technique'))
        .dx;
    expect(
      btnRight,
      greaterThan(screenWidth - 40),
      reason: 'New technique should hug the right edge',
    );
    // The whole cluster is right-anchored: the search box sits in the right
    // half (not parked next to the title on the left).
    final searchX = tester.getCenter(find.byIcon(FIcons.search)).dx;
    expect(
      searchX,
      greaterThan(screenWidth * 0.45),
      reason: 'search + filter should be right-anchored, not on the left',
    );
    expect(tester.takeException(), isNull);
  });

  testWidgets('tag filter dropdown lists tags and filters on select', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(1440, 900);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);

    await _pump(tester, [
      _t('1', 'Twang', ['EVT']),
      _t('2', 'Curbing', ['CVT']),
    ]);

    // Open the dropdown.
    await tester.tap(find.text('All tags'));
    await tester.pumpAndSettle();
    // The menu lists the tags (EVT also appears on a card, so findsWidgets).
    expect(find.text('CVT'), findsWidgets);

    // Selecting a tag filters the grid and closes the menu.
    await tester.tap(find.text('CVT').last);
    await tester.pumpAndSettle();
    expect(find.text('Curbing'), findsWidgets); // CVT technique stays
    expect(find.text('Twang'), findsNothing); // EVT technique filtered out
    expect(tester.takeException(), isNull);
  });
}
