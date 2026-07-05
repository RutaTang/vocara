import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:forui/forui.dart';
import 'package:provider/provider.dart';
import 'package:vocara/data/repositories/technique_repository.dart';
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/data/services/technique_media_service.dart';
import 'package:vocara/data/sync/sync_hashes.dart';
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/domain/models/technique_media.dart';
import 'package:vocara/ui/technique_library/view_model/technique_detail_view_model.dart';
import 'package:vocara/ui/technique_library/widgets/technique_detail_screen.dart';
import 'package:vocara/utils/result.dart';

class _FakeRepo implements TechniqueRepository {
  _FakeRepo(this.technique);
  Technique technique;

  @override
  Future<Result<Technique>> getTechnique(String id) async =>
      Result.ok(technique);

  @override
  Future<Result<Loaded<Technique>>> loadTechnique(String id) async =>
      Result.ok(Loaded(technique, const {}));

  @override
  Future<Result<List<Technique>>> listTechniques() async =>
      Result.ok([technique]);

  @override
  Future<Result<Technique>> saveTechnique(Technique t) async {
    technique = t;
    return Result.ok(t);
  }

  @override
  Future<Result<Loaded<Technique>>> saveTechniqueWithBase(
    Technique t, {
    required Map<String, String> baseHashes,
  }) async {
    technique = t;
    return Result.ok(Loaded(t, const {}));
  }

  @override
  Future<Result<void>> deleteTechnique(String id) async =>
      const Result.ok(null);
}

Technique _technique({
  bool body = true,
  List<TechniqueMedia> media = const [],
}) {
  final now = DateTime(2026, 6, 20);
  return Technique(
    id: 't1',
    name: 'Twang',
    aliases: const ['Edge'],
    tags: const ['EVT', 'Voice quality'],
    summary: body ? 'Bright carrying quality.' : '',
    description: body ? 'A long description. ' * 20 : '',
    cues: body ? 'Quack like a duck.' : '',
    media: media,
    createdAt: now,
    updatedAt: now,
  );
}

Future<void> _pump(WidgetTester tester, Technique technique) async {
  final vm = TechniqueDetailViewModel(
    repository: _FakeRepo(technique),
    mediaService: TechniqueMediaService(storage: LocalStorageService()),
    techniqueId: technique.id,
  );
  await tester.pumpWidget(
    MaterialApp(
      home: ChangeNotifierProvider.value(
        value: vm,
        child: const TechniqueDetailScreen(),
      ),
    ),
  );
  await tester.pump(); // let the load command resolve
  await tester.pump();
}

void main() {
  testWidgets('viewer with body + media lays out without overflow', (
    tester,
  ) async {
    final media = [
      TechniqueMedia(
        id: 'm1',
        path: '/tmp/a.m4a',
        kind: TechniqueMediaKind.audio,
        addedAt: DateTime(2026, 6, 20),
      ),
      TechniqueMedia(
        id: 'm2',
        path: '/tmp/b.mp4',
        kind: TechniqueMediaKind.video,
        addedAt: DateTime(2026, 6, 20),
      ),
    ];
    await _pump(tester, _technique(media: media));
    expect(tester.takeException(), isNull);
  });

  testWidgets('empty technique: view shows every field with placeholders', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(1200, 2200);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);

    await _pump(tester, _technique(body: false)); // no text, no media
    // Read view (Edit/Delete in header, no Cancel/Save).
    expect(find.text('Edit'), findsWidgets);
    expect(find.text('Delete'), findsOneWidget);
    expect(find.text('Cancel'), findsNothing);
    expect(find.text('Save'), findsNothing);
    // Every field stays visible; empty ones read "Empty content". The Examples
    // label shows too, but with no add/drop zone.
    expect(find.text('EXAMPLES'), findsOneWidget);
    expect(find.textContaining('Drag an audio'), findsNothing);
    expect(find.text('Empty content'), findsWidgets);
    expect(tester.takeException(), isNull);
  });

  testWidgets('tapping Edit opens the form with Cancel/Save in header', (
    tester,
  ) async {
    // Tall window so the whole edit form (incl. the Examples block) lays out.
    tester.view.physicalSize = const Size(1200, 2600);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);

    await _pump(tester, _technique(body: false)); // view mode
    await tester.tap(find.text('Edit').first);
    await tester.pump();

    // Edit mode shows the "Edit technique" title and drops the back chevron —
    // Cancel/Save are the exits.
    expect(find.text('Edit technique'), findsOneWidget);
    expect(find.byIcon(FIcons.chevronLeft), findsNothing);
    expect(find.text('Cancel'), findsOneWidget);
    expect(find.text('Save'), findsOneWidget);
    // Media is addable while editing too.
    expect(find.text('EXAMPLES'), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  testWidgets('view mode: header buttons hug the right edge', (tester) async {
    tester.view.physicalSize = const Size(1400, 900);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);

    await _pump(tester, _technique()); // has body -> view mode
    expect(find.text('Twang'), findsWidgets);
    expect(find.text('Edit'), findsOneWidget);
    expect(find.text('Delete'), findsOneWidget);

    // The action buttons sit flush right, not floating in the middle.
    final screenRight = tester.getSize(find.byType(MaterialApp)).width;
    final deleteRight = tester
        .getBottomRight(find.widgetWithText(GestureDetector, 'Delete').first)
        .dx;
    expect(
      deleteRight,
      greaterThan(screenRight - 60),
      reason: 'Delete button should hug the right edge',
    );

    expect(tester.takeException(), isNull);
  });

  testWidgets('view mode with clips: Examples shown but play-only', (
    tester,
  ) async {
    final media = [
      TechniqueMedia(
        id: 'm1',
        path: '/tmp/a.m4a',
        kind: TechniqueMediaKind.audio,
        addedAt: DateTime(2026, 6, 20),
      ),
    ];
    await _pump(tester, _technique(media: media));
    // The block shows, but with no add/drop zone (read-only).
    expect(find.text('EXAMPLES'), findsOneWidget);
    expect(find.textContaining('Drag an audio'), findsNothing);
    expect(tester.takeException(), isNull);
  });
}
