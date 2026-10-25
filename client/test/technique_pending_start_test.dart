// A recording start pends behind the mic permission prompt and the mic open
// (up to the ready timeout). isStarting flips at the tap, but the screen only
// sees what the view model notifies: these tests pin that the pending window
// is visible to it, and that nothing the take depends on can change in it.

import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:provider/provider.dart';
import 'package:vocara/data/repositories/technique_repository.dart';
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
  Future<Result<Technique>> saveTechnique(Technique t) async =>
      Result.ok(technique = t);

  @override
  Future<Result<Loaded<Technique>>> saveTechniqueWithBase(
    Technique t, {
    required Map<String, String> baseHashes,
  }) async => Result.ok(Loaded(technique = t, const {}));

  @override
  Future<Result<void>> deleteTechnique(String id) async =>
      const Result.ok(null);
}

class _NoAnalysis extends Fake implements MediaAnalysisService {
  @override
  Future<TechniqueAnalysis?> analyze(
    String path, {
    bool Function()? wanted,
  }) async => null;
}

/// Recorder fake whose [start] parks until the test resolves [pending] —
/// the permission prompt / mic open — with an outcome.
class _ParkedRecorder extends Fake implements MicRecordingController {
  bool recording = false;
  bool starting = false;
  Completer<MicRecordingStart>? pending;
  final ValueNotifier<double> _level = ValueNotifier(0);

  @override
  ValueNotifier<double> get level => _level;

  @override
  bool get isRecording => recording;

  @override
  bool get isStarting => starting;

  @override
  bool get feedingMonitor => false;

  @override
  Future<MicRecordingStart> start(RecordingPathStrategy destination) async {
    starting = true;
    final r = await (pending = Completer<MicRecordingStart>()).future;
    starting = false;
    recording = r.started;
    return r;
  }

  /// Arms the parked take in a fresh temp dir (the view model deletes a
  /// take's whole parent dir on cleanup).
  Future<void> arm() async {
    final dir = await Directory.systemTemp.createTemp('vocara_fake_take');
    pending!.complete(MicRecordingStart.started('${dir.path}/take.wav'));
  }

  @override
  void invalidatePendingStart() => starting = false;

  @override
  void dispose() => _level.dispose();
}

const _techniqueId = 'T1000000000000000000000000';

Technique _technique() {
  final now = DateTime(2026, 6, 20);
  return Technique(
    id: _techniqueId,
    name: 'Twang',
    media: [
      TechniqueMedia(
        id: 'M0',
        path: '/tmp/example-0.wav',
        kind: TechniqueMediaKind.audio,
        addedAt: now,
      ),
    ],
    createdAt: now,
    updatedAt: now,
  );
}

Future<TechniqueDetailViewModel> _loadedVm(_ParkedRecorder recorder) async {
  final vm = TechniqueDetailViewModel(
    repository: _FakeRepo(_technique()),
    mediaService: TechniqueMediaService(storage: LocalStorageService()),
    techniqueId: _techniqueId,
    analysisService: _NoAnalysis(),
    recorder: recorder,
  );
  addTearDown(vm.dispose);
  for (var i = 0; i < 200 && vm.technique == null; i++) {
    await Future<void>.delayed(const Duration(milliseconds: 5));
  }
  expect(vm.technique, isNotNull);
  return vm;
}

void main() {
  test('a pending practice start is announced, and the card holding its '
      'Stop cannot collapse under it', () async {
    // Regression: isStarting flipped without a notify, so during the
    // permission prompt the card still looked collapsible — collapsing it
    // armed the take with its Stop and meter hidden while the mic was hot.
    final recorder = _ParkedRecorder();
    final vm = await _loadedVm(recorder);
    var notified = 0;
    vm.addListener(() => notified++);

    final started = vm.startPractice();
    expect(recorder.starting, isTrue);
    expect(notified, 1, reason: 'the screen must see the pending start');

    vm.toggleExampleExpanded('M0');
    expect(vm.expandedExampleId, 'M0', reason: 'the take arms into it');

    await recorder.arm();
    expect(await started, isNull);
    expect(vm.isRecordingPractice, isTrue);
    expect(vm.expandedExampleId, 'M0');
  });

  test('a start that fails after pending is announced too', () async {
    // The failure path cleared isStarting silently: whatever the pending
    // start had disabled stayed disabled until something else notified.
    final recorder = _ParkedRecorder();
    final vm = await _loadedVm(recorder);
    final started = vm.startPractice();
    var notified = 0;
    vm.addListener(() => notified++);

    recorder.pending!.complete(
      const MicRecordingStart.failed(MicRecordingFailure.permissionDenied),
    );
    expect(await started, isNotNull);
    expect(recorder.starting, isFalse);
    expect(notified, greaterThan(0));
  });

  testWidgets('Save is disabled while an example recording start is pending', (
    tester,
  ) async {
    // Regression: Save looked live through the permission prompt, and a tap
    // there toasted "Couldn't save" (the save refuses a pending start).
    tester.view.physicalSize = const Size(1200, 2600);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);
    final recorder = _ParkedRecorder();
    final vm = TechniqueDetailViewModel(
      repository: _FakeRepo(_technique()),
      mediaService: TechniqueMediaService(storage: LocalStorageService()),
      techniqueId: _techniqueId,
      analysisService: _NoAnalysis(),
      recorder: recorder,
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
    await tester.pump();
    await tester.tap(find.text('Edit'));
    await tester.pump();
    StudioButton save() =>
        tester.widget<StudioButton>(find.widgetWithText(StudioButton, 'Save'));
    expect(save().enabled, isTrue);

    unawaited(vm.startExampleRecording());
    await tester.pump();
    expect(save().enabled, isFalse);

    recorder.pending!.complete(const MicRecordingStart.invalidated());
    await tester.pump();
    await tester.pump();
    expect(save().enabled, isTrue);
  });
}
