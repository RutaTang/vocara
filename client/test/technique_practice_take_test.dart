// The ephemeral practice take lives in the READ view only: the edit view has
// no practice recorder, no Stop control and no Reference/Practice switch. These
// tests pin the seams between the two — a take must never leak into (or be
// stranded by) an edit session.

import 'dart:async';
import 'dart:io';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:provider/provider.dart';
import 'package:vocara/data/repositories/technique_repository.dart';
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/data/services/media_analysis_service.dart';
import 'package:vocara/data/services/mic_recording_controller.dart';
import 'package:vocara/data/services/technique_media_service.dart';
import 'package:vocara/data/sync/sync_hashes.dart';
import 'package:vocara/domain/models/spectrogram.dart';
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/domain/models/technique_analysis.dart';
import 'package:vocara/domain/models/technique_media.dart';
import 'package:vocara/domain/models/voice_metrics.dart';
import 'package:vocara/ui/core/widgets/studio_button.dart';
import 'package:vocara/ui/technique_library/view_model/technique_detail_view_model.dart';
import 'package:vocara/ui/technique_library/widgets/technique_detail_screen.dart';
import 'package:vocara/utils/result.dart';

class _FakeRepo implements TechniqueRepository {
  _FakeRepo(this.technique);
  Technique technique;
  int baseSaves = 0;

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
    baseSaves++;
    technique = t;
    return Result.ok(Loaded(t, const {}));
  }

  @override
  Future<Result<void>> deleteTechnique(String id) async =>
      const Result.ok(null);
}

/// Analysis fake that returns a FRESH canned analysis per call, so a test can
/// tell the practice take's analysis from the reference's by identity. While
/// [gate] is set, every call parks on it (a slow DSP pass).
class _FreshAnalysis extends Fake implements MediaAnalysisService {
  Completer<void>? gate;

  @override
  Future<TechniqueAnalysis?> analyze(String path) async {
    await gate?.future;
    return _analysis();
  }
}

/// Recorder fake: [start] writes a WAV stub into its own temp dir and [stop]
/// reports a clean take. [starting] models a start still pending behind the
/// permission dialog / temp-dir awaits.
class _FakeRecorder extends Fake implements MicRecordingController {
  bool recording = false;
  bool starting = false;

  final ValueNotifier<double> _level = ValueNotifier(0);
  String? _path;

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
    final dir = await Directory.systemTemp.createTemp('vocara_fake_take');
    final path = p.join(dir.path, 'take.wav');
    await File(path).writeAsBytes(const [1, 2, 3]);
    _path = path;
    recording = true;
    return MicRecordingStart.started(path);
  }

  @override
  MicRecordingStop? stop() {
    if (!recording) return null;
    recording = false;
    return (path: _path!, clean: true, droppedFrames: 0);
  }

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

TechniqueAnalysis _analysis() {
  Float32List flat(double v) => Float32List(4)..fillRange(0, 4, v);
  final metrics = VoiceMetrics(
    frames: 4,
    hop: 512,
    fftSize: 2048,
    sampleRate: 44100,
    ring: flat(0),
    tilt: flat(0),
    cpp: flat(0),
    loudness: flat(-20),
    f0: flat(200),
    f1: flat(double.nan),
    f2: flat(double.nan),
    f3: flat(double.nan),
    h1h2: flat(double.nan),
    active: Uint8List(4),
    ringAvg: 0,
    tiltAvg: 0,
    cppAvg: 0,
    peakDbfs: -6,
    clippedFraction: 0,
    snrDb: double.nan,
  );
  return TechniqueAnalysis(
    metrics: metrics,
    spectrogram: Spectrogram(
      frames: 2,
      bins: 2,
      maxHz: 5000,
      sampleRate: 44100,
      fftSize: 2048,
      hop: 512,
      db: Float32List(4),
      minDb: -80,
      maxDb: 0,
    ),
    ltas: Ltas(
      db: Float32List(4),
      binHz: 21.5,
      maxHz: 5000,
      maxDb: -20,
      alphaRatio: -12,
      singersFormantDb: 3,
      frameCount: 2,
    ),
    profile: const VoiceProfile(
      lowMidi: 48,
      highMidi: 72,
      tessLoMidi: 55,
      tessHiMidi: 65,
      passaggioMidi: null,
      passaggioJumpDb: 0,
      softDb: -30,
      loudDb: -10,
      medianDb: -20,
    ),
    register: RegisterMap(
      score: Float32List(4),
      chestPct: 50,
      mixPct: 30,
      headPct: 20,
      voicedFrames: 4,
    ),
    durationMs: 1000,
  );
}

const _techniqueId = 'T1000000000000000000000000';

Technique _technique({List<String> examples = const []}) {
  final now = DateTime(2026, 6, 20);
  return Technique(
    id: _techniqueId,
    name: 'Twang',
    media: [
      for (final (i, path) in examples.indexed)
        TechniqueMedia(
          id: 'M$i',
          path: path,
          kind: TechniqueMediaKind.audio,
          addedAt: now,
        ),
    ],
    createdAt: now,
    updatedAt: now,
  );
}

/// A loaded view model over [repo] (no screen), for the VM-level tests.
Future<TechniqueDetailViewModel> _loadedVm(
  _FakeRepo repo,
  _FreshAnalysis analysis,
) async {
  final vm = TechniqueDetailViewModel(
    repository: repo,
    mediaService: TechniqueMediaService(storage: LocalStorageService()),
    techniqueId: _techniqueId,
    analysisService: analysis,
    recorder: _FakeRecorder(),
  );
  addTearDown(vm.dispose);
  for (var i = 0; i < 200 && vm.technique == null; i++) {
    await Future<void>.delayed(const Duration(milliseconds: 5));
  }
  expect(vm.technique, isNotNull);
  return vm;
}

Future<TechniqueDetailViewModel> _pumpScreen(
  WidgetTester tester,
  _FakeRepo repo,
  _FakeRecorder recorder,
) async {
  // Tall window so the whole edit form lays out.
  tester.view.physicalSize = const Size(1200, 2600);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.resetPhysicalSize);

  final vm = TechniqueDetailViewModel(
    repository: repo,
    mediaService: TechniqueMediaService(storage: LocalStorageService()),
    techniqueId: _techniqueId,
    analysisService: _FreshAnalysis(),
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
  await tester.pump(); // let the load command resolve
  await tester.pump();
  return vm;
}

StudioButton _button(WidgetTester tester, String label) =>
    tester.widget<StudioButton>(find.widgetWithText(StudioButton, label));

void main() {
  testWidgets('Edit is refused while a practice take is recording or '
      'starting', (tester) async {
    // Regression: Edit stayed live during a practice take. The edit view has
    // no practice Stop control, its Save refuses while the mic is hot (every
    // attempt toasted "Couldn't save"), and Cancel's draft discard silently
    // aborted the take — the user's only way out threw the take away.
    final repo = _FakeRepo(_technique());
    final recorder = _FakeRecorder();
    final vm = await _pumpScreen(tester, repo, recorder);
    expect(_button(tester, 'Edit').enabled, isTrue);

    await tester.runAsync(vm.startPractice);
    await tester.pump();
    expect(vm.isRecordingPractice, isTrue);
    expect(
      _button(tester, 'Edit').enabled,
      isFalse,
      reason: 'stop the take first: the editor has no practice controls',
    );
    await tester.tap(find.text('Edit'));
    await tester.pump();
    expect(find.text('Edit technique'), findsNothing);
    expect(vm.isRecordingPractice, isTrue, reason: 'the take is untouched');

    await tester.runAsync(vm.stopPractice);
    await tester.pump();
    expect(_button(tester, 'Edit').enabled, isTrue);

    // A start still pending behind its awaits notifies nothing, so the button
    // can look live — the tap itself must still be refused, or the take arms
    // inside the editor with no Stop.
    recorder.starting = true;
    await tester.tap(find.text('Edit'));
    await tester.pump();
    expect(find.text('Edit technique'), findsNothing);

    recorder.starting = false;
    await tester.tap(find.text('Edit'));
    await tester.pump();
    expect(find.text('Edit technique'), findsOneWidget);
    expect(repo.baseSaves, 0);
    expect(tester.takeException(), isNull);
  });

  test('opening the editor points the analysis back at the reference, even '
      'when the practice analysis lands after it', () async {
    // Regression: the edit view's example card has no Reference/Practice
    // switch, yet beginEdit kept the practice subject — the card showed the
    // practice take's analysis, and playback/scrub (which only drive the
    // reference) no longer moved it.
    final analysis = _FreshAnalysis();
    final vm = await _loadedVm(
      _FakeRepo(_technique(examples: ['/tmp/ref-a.wav'])),
      analysis,
    );
    await vm.ensureReferenceAnalysis();
    expect(await vm.startPractice(), isNull);
    await vm.stopPractice();
    expect(vm.analysisSubject, TechniqueAnalysisSubject.practice);

    vm.beginMediaDraft();
    vm.beginEdit();
    expect(vm.analysisSubject, TechniqueAnalysisSubject.reference);
    expect(
      identical(vm.selectedMetrics, vm.referenceAnalysis!.metrics),
      isTrue,
    );
    vm.endEdit();

    // Edit tapped while a take's analysis is still running: the take landing
    // afterwards must not flip the open editor over to it either.
    analysis.gate = Completer<void>();
    expect(await vm.startPractice(), isNull);
    final stopping = vm.stopPractice();
    expect(vm.practiceAnalysisLoading, isTrue);
    vm.beginMediaDraft();
    vm.beginEdit();
    analysis.gate!.complete();
    await stopping;
    expect(vm.practiceAnalysis, isNotNull, reason: 'the take itself is kept');
    expect(vm.analysisSubject, TechniqueAnalysisSubject.reference);
    expect(
      identical(vm.selectedMetrics, vm.referenceAnalysis!.metrics),
      isTrue,
    );
  });
}
