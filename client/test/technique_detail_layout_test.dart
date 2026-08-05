import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:forui/forui.dart';
import 'package:provider/provider.dart';
import 'package:vocara/data/repositories/technique_repository.dart';
import 'package:vocara/data/services/graph_audio_playback_service.dart'
    show MediaTooLongException;
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/data/services/media_analysis_service.dart';
import 'package:vocara/data/services/technique_media_service.dart';
import 'package:vocara/data/sync/sync_hashes.dart';
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/domain/models/technique_analysis.dart';
import 'package:vocara/domain/models/technique_media.dart';
import 'package:vocara/ui/core/widgets/studio_button.dart';
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

/// A repo whose [saveTechniqueWithBase] parks on [gate] until the test releases
/// it — models a slow disk so a save can be observed mid-flight.
class _GatedSaveRepo extends _FakeRepo {
  _GatedSaveRepo(super.technique);

  final Completer<void> gate = Completer<void>();
  int saveWithBaseCalls = 0;

  @override
  Future<Result<Loaded<Technique>>> saveTechniqueWithBase(
    Technique t, {
    required Map<String, String> baseHashes,
  }) async {
    saveWithBaseCalls++;
    await gate.future;
    return super.saveTechniqueWithBase(t, baseHashes: baseHashes);
  }
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

  test('a MediaTooLongException analysis failure surfaces its cause via '
      'referenceAnalysisFailureMessage', () async {
    // Regression: the decode-length cap throws with user-presentable copy,
    // but the VM's generic catch used to reduce it to the anonymous
    // "couldn't analyze" state — the message must reach the failure hint.
    final media = TechniqueMedia(
      id: 'm1',
      path: '/tmp/hours-long.m4a',
      kind: TechniqueMediaKind.audio,
      addedAt: DateTime(2026, 6, 20),
    );
    final vm = TechniqueDetailViewModel(
      repository: _FakeRepo(_technique(media: [media])),
      mediaService: TechniqueMediaService(storage: LocalStorageService()),
      techniqueId: 't1',
      analysisService: _CapTrippingAnalysis(),
    );
    addTearDown(vm.dispose);
    for (var i = 0; i < 200 && vm.technique == null; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }
    expect(vm.technique, isNotNull);
    expect(vm.referenceMedia?.path, media.path);

    await vm.ensureReferenceAnalysis();

    expect(vm.referenceAnalysisFailed, isTrue);
    expect(vm.referenceAnalysisFailureMessage, contains('30 minutes'));
  });

  test('save() is a no-op while a save is already in flight', () async {
    // Regression: a double-click on Save used to run two commits against the
    // same stale base hashes. The second call must not reach the repository.
    final repo = _GatedSaveRepo(_technique());
    final vm = TechniqueDetailViewModel(
      repository: repo,
      mediaService: TechniqueMediaService(storage: LocalStorageService()),
      techniqueId: 't1',
    );
    addTearDown(vm.dispose);
    for (var i = 0; i < 200 && vm.technique == null; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }
    expect(vm.technique, isNotNull);

    final edited = vm.technique!.copyWith(summary: 'changed');
    final first = vm.save(edited);
    // The flag flips synchronously, so a re-entrant call sees it immediately.
    expect(vm.saving, isTrue);
    expect(await vm.save(edited), isFalse, reason: 'second save is a no-op');
    expect(repo.saveWithBaseCalls, 1, reason: 'one repository write only');

    repo.gate.complete();
    expect(await first, isTrue, reason: 'the original save still persists');
    expect(vm.saving, isFalse, reason: 'flag clears when the save resolves');
  });

  test('media mutations during an in-flight save are no-ops', () async {
    // Regression: the commit snapshots effectiveMedia when it starts, so a
    // clip removed/renamed/added during the await used to be silently thrown
    // away (or worse, un-happen) when the save landed and cleared the draft.
    final media = TechniqueMedia(
      id: 'm1',
      path: '/tmp/a.m4a',
      kind: TechniqueMediaKind.audio,
      addedAt: DateTime(2026, 6, 20),
    );
    final repo = _GatedSaveRepo(_technique(media: [media]));
    final vm = TechniqueDetailViewModel(
      repository: repo,
      mediaService: TechniqueMediaService(storage: LocalStorageService()),
      techniqueId: 't1',
    );
    addTearDown(vm.dispose);
    for (var i = 0; i < 200 && vm.technique == null; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }
    expect(vm.technique, isNotNull);

    vm.beginMediaDraft();
    final first = vm.save(vm.technique!.copyWith(summary: 'changed'));
    expect(vm.saving, isTrue);

    // Every mutation entry point must refuse while the commit is airborne.
    await vm.removeMedia('m1');
    await vm.renameMedia('m1', 'sneaky caption');
    await vm.addMedia(['/tmp/never-imported.wav']);
    expect(vm.hasMediaDraft, isFalse, reason: 'nothing staged mid-save');
    expect(vm.importingMedia, isFalse, reason: 'no import launched mid-save');
    expect(vm.canRecordExample, isFalse, reason: 'no take can arm mid-save');
    expect(vm.canRecordPractice, isFalse);

    repo.gate.complete();
    expect(await first, isTrue);
    // The saved result matches the pre-mutation state: the clip survives,
    // unrenamed.
    expect(repo.technique.media, hasLength(1));
    expect(repo.technique.media.single.id, 'm1');
    expect(repo.technique.media.single.label, isEmpty);
  });

  testWidgets('Save and Cancel are disabled while the save is in flight', (
    tester,
  ) async {
    // Tall window so the whole edit form lays out (as in the Edit test above).
    tester.view.physicalSize = const Size(1200, 2600);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);

    final repo = _GatedSaveRepo(_technique(body: false));
    final vm = TechniqueDetailViewModel(
      repository: repo,
      mediaService: TechniqueMediaService(storage: LocalStorageService()),
      techniqueId: 't1',
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

    await tester.tap(find.text('Edit').first);
    await tester.pump();

    StudioButton button(String label) =>
        tester.widget<StudioButton>(find.widgetWithText(StudioButton, label));
    expect(button('Save').enabled, isTrue);
    expect(button('Cancel').enabled, isTrue);

    bool allFieldsEnabled() {
      final fields = tester.widgetList<TextField>(find.byType(TextField));
      expect(fields, isNotEmpty);
      return fields.every((f) => f.enabled ?? true);
    }

    expect(allFieldsEnabled(), isTrue);

    // Start a save; the repo parks on the gate so it stays in flight.
    await tester.tap(find.text('Save'));
    await tester.pump();
    expect(button('Save').enabled, isFalse, reason: 'no double commit');
    expect(
      button('Cancel').enabled,
      isFalse,
      reason: 'Cancel -> Discard would delete files the commit references',
    );
    // The FORM freezes too: text typed during the await would be thrown away
    // when the save lands and the screen drops out of edit mode.
    expect(
      allFieldsEnabled(),
      isFalse,
      reason: 'every text field is frozen while the save commits',
    );
    // A tap on the disabled Cancel must not open the discard dialog (which
    // would tear down the edit session mid-save).
    await tester.tap(find.text('Cancel'));
    await tester.pump();
    expect(find.text('Discard unsaved changes?'), findsNothing);

    // Release the save: it completes, the screen leaves edit mode.
    repo.gate.complete();
    await tester.pumpAndSettle();
    expect(repo.saveWithBaseCalls, 1);
    expect(find.text('Cancel'), findsNothing, reason: 'back in the read view');
    expect(tester.takeException(), isNull);
  });
}

/// Analysis fake that always trips the decode-length cap, as a real
/// hours-long reference clip would.
class _CapTrippingAnalysis extends Fake implements MediaAnalysisService {
  @override
  Future<TechniqueAnalysis?> analyze(String path) async =>
      throw const MediaTooLongException(Duration(minutes: 30));
}
