// Reference analyses queue behind the app-wide analysis gate (two slots,
// first come first served). One the user moved away from while it waited —
// another example selected, the screen left — must be dropped when the gate
// admits it, not decoded in full ahead of the analyses still wanted.

import 'dart:async';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/repositories/technique_repository.dart';
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/data/services/media_analysis_service.dart';
import 'package:vocara/data/services/technique_media_service.dart';
import 'package:vocara/data/sync/sync_hashes.dart';
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/domain/models/technique_analysis.dart';
import 'package:vocara/domain/models/technique_media.dart';
import 'package:vocara/ui/technique_library/view_model/technique_detail_view_model.dart';
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

/// Analysis fake modelling the gate: every call waits for [admit], then asks
/// `wanted` (when given) exactly as the real service does on admission, and
/// records which paths it went on to decode.
class _QueuedAnalysis extends Fake implements MediaAnalysisService {
  final Completer<void> admit = Completer<void>();
  final decoded = <String>[];

  @override
  Future<TechniqueAnalysis?> analyze(
    String path, {
    bool Function()? wanted,
  }) async {
    await admit.future;
    if (wanted != null && !wanted()) return null;
    decoded.add(path);
    return null;
  }
}

const _techniqueId = 'T1000000000000000000000000';

Future<TechniqueDetailViewModel> _loadedVm(_QueuedAnalysis analysis) async {
  final now = DateTime(2026, 6, 20);
  final vm = TechniqueDetailViewModel(
    repository: _FakeRepo(
      Technique(
        id: _techniqueId,
        name: 'Twang',
        media: [
          for (final name in ['a', 'b', 'c'])
            TechniqueMedia(
              id: name.toUpperCase(),
              path: '/tmp/$name.wav',
              kind: TechniqueMediaKind.audio,
              addedAt: now,
            ),
        ],
        createdAt: now,
        updatedAt: now,
      ),
    ),
    mediaService: TechniqueMediaService(storage: LocalStorageService()),
    techniqueId: _techniqueId,
    analysisService: analysis,
  );
  for (var i = 0; i < 200 && vm.technique == null; i++) {
    await Future<void>.delayed(const Duration(milliseconds: 5));
  }
  expect(vm.technique, isNotNull);
  return vm;
}

void main() {
  test('clicking through examples decodes only the one still selected when '
      'the gate admits', () async {
    // Regression: every example clicked queued a full decode + DSP pass, and
    // the one finally selected (and a practice take's analysis after it)
    // waited behind references nobody was looking at any more.
    final analysis = _QueuedAnalysis();
    final vm = await _loadedVm(analysis);
    addTearDown(vm.dispose);

    final first = vm.ensureReferenceAnalysis(); // A, the default selection
    vm.selectExample('B');
    vm.selectExample('C');
    analysis.admit.complete();
    await first;
    await pumpEventQueue();

    expect(analysis.decoded, ['/tmp/c.wav']);
    vm.selectExample('A');
    expect(
      vm.referenceAnalysisFailed,
      isFalse,
      reason: 'a skipped analysis must not latch the example as failed',
    );
    await vm.ensureReferenceAnalysis();
    expect(analysis.decoded, ['/tmp/c.wav', '/tmp/a.wav']);
  });

  test('an analysis still queued when the screen goes away is never '
      'decoded', () async {
    final analysis = _QueuedAnalysis();
    final vm = await _loadedVm(analysis);

    final pending = vm.ensureReferenceAnalysis();
    vm.dispose();
    analysis.admit.complete();
    await pending;

    expect(analysis.decoded, isEmpty);
  });

  test('an example whose analysis failed for no stated cause can be '
      'analyzed again', () async {
    // The failure was latched for the life of the page.
    final analysis = _QueuedAnalysis()..admit.complete();
    final vm = await _loadedVm(analysis);
    addTearDown(vm.dispose);

    await vm.ensureReferenceAnalysis();
    expect(vm.referenceAnalysisFailed, isTrue);
    expect(vm.canRetryReferenceAnalysis, isTrue);
    await vm.ensureReferenceAnalysis(); // latched: not asked again
    expect(analysis.decoded, ['/tmp/a.wav']);

    await vm.retryReferenceAnalysis();
    expect(analysis.decoded, ['/tmp/a.wav', '/tmp/a.wav']);
  });
}
