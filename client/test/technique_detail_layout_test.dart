import 'dart:async';
import 'dart:io';

import 'package:desktop_drop/desktop_drop.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:forui/forui.dart';
import 'package:path/path.dart' as p;
import 'package:provider/provider.dart';
import 'package:vocara/data/repositories/technique_repository.dart';
import 'package:vocara/data/services/audio_exporter.dart';
import 'package:vocara/data/services/audio_file_service.dart';
import 'package:vocara/data/services/graph_audio_playback_service.dart'
    show MediaTooLongException;
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/data/services/media_analysis_service.dart';
import 'package:vocara/data/services/mic_recording_controller.dart';
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
/// it — models a slow disk so a save can be observed mid-flight. With
/// [failNext] set, the released save fails instead (and clears the flag).
class _GatedSaveRepo extends _FakeRepo {
  _GatedSaveRepo(super.technique);

  final Completer<void> gate = Completer<void>();
  int saveWithBaseCalls = 0;
  bool failNext = false;

  @override
  Future<Result<Loaded<Technique>>> saveTechniqueWithBase(
    Technique t, {
    required Map<String, String> baseHashes,
  }) async {
    saveWithBaseCalls++;
    await gate.future;
    if (failNext) {
      failNext = false;
      return Result.error(Exception('simulated write failure'));
    }
    return super.saveTechniqueWithBase(t, baseHashes: baseHashes);
  }
}

/// A repo whose [loadTechnique] parks on [gate] and then fails — a technique
/// deleted underneath the route, or an unreadable row, on a slow disk.
class _FailingLoadRepo extends _FakeRepo {
  _FailingLoadRepo(super.technique);

  final Completer<void> gate = Completer<void>();

  @override
  Future<Result<Loaded<Technique>>> loadTechnique(String id) async {
    await gate.future;
    return Result.error(Exception('simulated read failure'));
  }
}

/// Storage rooted in a temp folder: the app's own storage, which an export
/// must never write into, lives there.
class _RootedStorage extends LocalStorageService {
  _RootedStorage(this.root);
  final Directory root;

  @override
  Future<String> assetPath(String collection) async =>
      p.join(root.path, 'app', collection);
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

Future<TechniqueDetailViewModel> _pump(
  WidgetTester tester,
  Technique technique,
) async {
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
  return vm;
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

  testWidgets('a failed load shows the error state, not an endless spinner', (
    tester,
  ) async {
    // Regression: the screen watches only the view model, and the load's
    // failure path notified nothing but the (unwatched) command — the
    // spinner, keyed on load.running, was never rebuilt away.
    final repo = _FailingLoadRepo(_technique());
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
    await tester.pump();
    expect(find.byType(CircularProgressIndicator), findsOneWidget);

    // The read fails only once the screen is up and showing the spinner.
    repo.gate.complete();
    await tester.pump();
    await tester.pump();

    expect(vm.load.error, isTrue);
    expect(find.byType(CircularProgressIndicator), findsNothing);
    expect(find.text('This technique could not be loaded.'), findsOneWidget);
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

  // Regression guard: the drop zone used to filter unsupported files out
  // BEFORE calling the view model, so a drop of only unsupported files did
  // nothing at all — no import, and the notice the VM already had for exactly
  // this case was unreachable.
  testWidgets('dropping only unsupported files reports the skip', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(1200, 2600);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);

    final vm = await _pump(tester, _technique(body: false));
    await tester.tap(find.text('Edit').first);
    await tester.pump();

    final finder = find.byType(DropTarget);
    tester.widget<DropTarget>(finder).onDragDone!(
      DropDoneDetails(
        files: [DropItemFile('/tmp/notes.pdf')],
        localPosition: Offset.zero,
        // On the zone itself: a drop is taken only where it can be seen.
        globalPosition: tester.getCenter(finder),
      ),
    );
    await tester.pump();
    await tester.pump();

    expect(vm.effectiveMedia, isEmpty);
    expect(
      find.text('No supported audio or video files in the drop.'),
      findsOneWidget,
      reason: 'a drop that imports nothing must say so',
    );
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

  testWidgets('an example can be exported, from the read view too', (
    tester,
  ) async {
    // A recorded example was reachable only by digging through the app's
    // container, its file named by a hash.
    final dir = Directory.systemTemp.createTempSync('vocara_export');
    addTearDown(() => dir.deleteSync(recursive: true));
    final source = File(p.join(dir.path, 'take.wav'))
      ..writeAsBytesSync(const [1, 2, 3, 4]);
    final dest = p.join(dir.path, 'out', 'exported.wav');
    Directory(p.dirname(dest)).createSync();
    final suggested = <String>[];
    final media = [
      TechniqueMedia(
        id: 'm1',
        path: source.path,
        kind: TechniqueMediaKind.audio,
        label: 'Recording 1',
        addedAt: DateTime(2026, 6, 20),
      ),
    ];
    late TechniqueDetailViewModel vm;
    await tester.runAsync(() async {
      vm = TechniqueDetailViewModel(
        repository: _FakeRepo(_technique(media: media)),
        mediaService: TechniqueMediaService(storage: LocalStorageService()),
        techniqueId: 't1',
        exporter: AudioExporter(
          audioFiles: AudioFileService(storage: _RootedStorage(dir)),
          pickSaveLocation: (name) async {
            suggested.add(name);
            return dest;
          },
        ),
      );
      for (var i = 0; i < 200 && vm.technique == null; i++) {
        await Future<void>.delayed(const Duration(milliseconds: 5));
      }
    });
    addTearDown(vm.dispose);
    await tester.pumpWidget(
      MaterialApp(
        home: ChangeNotifierProvider.value(
          value: vm,
          child: const TechniqueDetailScreen(),
        ),
      ),
    );
    await tester.pump();
    expect(find.byIcon(FIcons.download), findsOneWidget);

    await tester.runAsync(() => vm.exportMedia('m1'));
    expect(suggested, ['Twang - Recording 1.wav']);
    expect(File(dest).readAsBytesSync(), [1, 2, 3, 4]);
    expect(vm.recordingNotice, 'Exported to $dest');
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

  test('the reverse races are refused too: no second save and no draft '
      'discard while the commit is airborne', () async {
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
    // Stage a rename so the draft is non-empty when the save launches (a
    // rename touches no files, so the commit's cleanup stays IO-free in this
    // harness): the discard below would clear it if the guard failed.
    await vm.renameMedia('m1', 'kept label');
    expect(vm.hasMediaDraft, isTrue);
    final first = vm.save(vm.technique!.copyWith(summary: 'changed'));
    expect(vm.saving, isTrue);

    // A racing second save must refuse against the same base…
    expect(await vm.save(vm.technique!.copyWith(summary: 'racer')), isFalse);
    // …and a direct discard (Cancel is disabled, but the method is callable)
    // must be a no-op while the commit is persisting the draft's outcome.
    await vm.discardMediaDraft();
    expect(
      vm.hasMediaDraft,
      isTrue,
      reason: 'the airborne save owns the draft; discard must not clear it',
    );

    repo.gate.complete();
    expect(await first, isTrue);
    // Exactly the FIRST save persisted, with the staged rename applied.
    expect(repo.technique.summary, 'changed');
    expect(repo.technique.media.single.label, 'kept label');
    expect(vm.hasMediaDraft, isFalse, reason: 'the save cleared its draft');
  });

  testWidgets('saving with the name cleared says so and keeps editing', (
    tester,
  ) async {
    // Save kept the old name and closed the editor as if the cleared one
    // had been saved.
    tester.view.physicalSize = const Size(1200, 2600);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);

    final vm = await _pump(tester, _technique(body: false));
    await tester.tap(find.text('Edit').first);
    await tester.pump();
    final name = find.byWidgetPredicate(
      (w) => w is TextField && w.controller?.text == 'Twang',
    );
    await tester.enterText(name, '  ');
    await tester.tap(find.text('Save'));
    await tester.pump();

    expect(find.text('Give the technique a name.'), findsOneWidget);
    expect(find.text('Edit technique'), findsOneWidget);
    expect(vm.technique!.name, 'Twang');
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

  test('an import abandoned by Cancel clears its busy state for the next '
      'edit session', () async {
    // Regression: the abandoned branch reset importingMedia without notifying,
    // so the screen kept the import spinner and the disabled Save/add/Record
    // it last saw — into the NEXT edit session, until something else
    // happened to rebuild it.
    final media = _GatedImportMedia();
    final vm = TechniqueDetailViewModel(
      repository: _FakeRepo(_technique()),
      mediaService: media,
      techniqueId: 't1',
    );
    addTearDown(vm.dispose);
    for (var i = 0; i < 200 && vm.technique == null; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }
    expect(vm.technique, isNotNull);
    bool? shownImporting;
    vm.addListener(() => shownImporting = vm.importingMedia);

    vm.beginMediaDraft();
    vm.beginEdit();
    final importing = vm.addMedia(['/tmp/dropped.wav']);
    expect(shownImporting, isTrue);
    // Cancel, then Edit again while the copy is still parked.
    await vm.discardMediaDraft();
    vm.endEdit();
    vm.beginMediaDraft();
    vm.beginEdit();

    media.gate.complete();
    await importing;
    expect(vm.importingMedia, isFalse);
    expect(
      shownImporting,
      isFalse,
      reason: 'the screen must hear that the import is over',
    );
    expect(vm.effectiveMedia, isEmpty, reason: 'the orphan is not staged');
    expect(media.deleted, ['/sandbox/dropped.wav']);
  });

  test('a staged clip is listed once while the save commits, and once again '
      'after the save fails', () async {
    // Regression: the commit's optimistic aggregate already carries the
    // staged additions while the draft still holds them, so effectiveMedia
    // listed every staged clip twice for the whole commit (an extra card,
    // and an extra audio player per duplicate).
    final media = _GatedImportMedia()..gate.complete();
    final repo = _GatedSaveRepo(_technique())..failNext = true;
    final vm = TechniqueDetailViewModel(
      repository: repo,
      mediaService: media,
      techniqueId: 't1',
    );
    addTearDown(vm.dispose);
    for (var i = 0; i < 200 && vm.technique == null; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }
    expect(vm.technique, isNotNull);
    vm.beginMediaDraft();
    vm.beginEdit();
    await vm.addMedia(['/tmp/clip.wav']);
    final staged = vm.effectiveMedia.single;

    final failing = vm.save(vm.technique!);
    expect(vm.saving, isTrue);
    expect(vm.effectiveMedia.map((m) => m.id), [staged.id]);
    repo.gate.complete();
    expect(await failing, isFalse);
    // The failed save reverted the aggregate; the clip is still staged — and
    // still listed exactly once — for the retry.
    expect(vm.effectiveMedia.map((m) => m.id), [staged.id]);
    expect(vm.hasMediaDraft, isTrue);

    expect(await vm.save(vm.technique!), isTrue);
    expect(vm.effectiveMedia.map((m) => m.id), [staged.id]);
    expect(vm.hasMediaDraft, isFalse);
    expect(repo.technique.media.map((m) => m.id), [staged.id]);
  });

  testWidgets('Save while an example take records keeps Cancel and the form '
      'frozen until the take and the edits are committed', (tester) async {
    // Regression: Save stopped the take and awaited its import with Cancel
    // still live. Cancel → Discard in that window tore the form down, and the
    // resuming Save then dereferenced the disposed controllers (or, with the
    // dialog still open when the import landed, committed anyway).
    tester.view.physicalSize = const Size(1200, 2600);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);

    final repo = _FakeRepo(_technique(body: false));
    final media = _GatedImportMedia();
    final vm = TechniqueDetailViewModel(
      repository: repo,
      mediaService: media,
      techniqueId: 't1',
      recorder: _FakeRecorder(),
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
    await tester.enterText(find.byType(TextField).first, 'Twang, revised');
    expect(await vm.startExampleRecording(), isNull);
    await tester.pump();
    expect(vm.isRecordingExample, isTrue);

    StudioButton button(String label) =>
        tester.widget<StudioButton>(find.widgetWithText(StudioButton, label));

    // Save: the take stops and its import parks on the gate.
    await tester.tap(find.text('Save'));
    await tester.pump();
    expect(vm.isRecordingExample, isFalse, reason: 'Save released the mic');
    expect(vm.savingExample, isTrue, reason: 'the take is still importing');
    expect(
      button('Cancel').enabled,
      isFalse,
      reason: 'a Discard now would tear the form down under the save',
    );
    expect(button('Save').enabled, isFalse);
    expect(
      tester
          .widgetList<TextField>(find.byType(TextField))
          .every((f) => f.enabled == false),
      isTrue,
      reason: 'the edits were already read for this save',
    );
    await tester.tap(find.text('Cancel'));
    await tester.pump();
    expect(find.text('Discard unsaved changes?'), findsNothing);

    // The import lands (its temp-dir cleanup is real file IO), then the
    // commit: the take AND the typed edit persist, and the editor closes.
    media.gate.complete();
    for (var i = 0; i < 200 && repo.technique.media.isEmpty; i++) {
      await tester.pump();
      await tester.runAsync(
        () => Future<void>.delayed(const Duration(milliseconds: 5)),
      );
    }
    await tester.pump();
    expect(repo.technique.name, 'Twang, revised');
    expect(repo.technique.media.map((m) => m.path), ['/sandbox/take.wav']);
    expect(find.text('Cancel'), findsNothing, reason: 'back in the read view');
    expect(tester.takeException(), isNull);
  });
}

/// Analysis fake that always trips the decode-length cap, as a real
/// hours-long reference clip would.
class _CapTrippingAnalysis extends Fake implements MediaAnalysisService {
  @override
  Future<TechniqueAnalysis?> analyze(
    String path, {
    bool Function()? wanted,
  }) async => throw const MediaTooLongException(Duration(minutes: 30));
}

/// Recorder fake that touches no disk: [start] hands out a path in a folder
/// that does not exist, and [stop] reports a clean take there. The quit-gate
/// bracket just runs its work (the quit itself is exercised elsewhere).
class _FakeRecorder extends Fake implements MicRecordingController {
  bool recording = false;

  final ValueNotifier<double> _level = ValueNotifier(0);

  static const _path = '/tmp/vocara_no_such_take_dir/take.wav';

  @override
  ValueNotifier<double> get level => _level;

  @override
  bool get isRecording => recording;

  @override
  bool get isStarting => false;

  @override
  bool get feedingMonitor => false;

  @override
  Future<MicRecordingStart> start(RecordingPathStrategy destination) async {
    recording = true;
    return const MicRecordingStart.started(_path);
  }

  @override
  MicRecordingStop? stop() {
    if (!recording) return null;
    recording = false;
    return (path: _path, clean: true, droppedFrames: 0);
  }

  @override
  Future<void> guardStopFinalize(Future<void> Function() work) => work();

  @override
  Future<void> abort() async {
    recording = false;
  }

  @override
  void invalidatePendingStart() {}

  @override
  void dispose() {
    _level.dispose();
  }
}

/// A media service whose [importMedia] parks on [gate] (a slow copy) and
/// records deletions instead of touching the disk.
class _GatedImportMedia extends TechniqueMediaService {
  _GatedImportMedia() : super(storage: LocalStorageService());

  final Completer<void> gate = Completer<void>();
  final deleted = <String>[];

  @override
  Future<String> importMedia({
    required String techniqueId,
    required String sourcePath,
  }) async {
    await gate.future;
    return '/sandbox/${sourcePath.split('/').last}';
  }

  @override
  Future<void> deleteMedia(String path) async => deleted.add(path);
}
