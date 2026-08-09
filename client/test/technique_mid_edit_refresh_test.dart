// A remote change landing WHILE the technique editor is open must not advance
// the base hashes the next save merges against.
//
// The editor seeds its text controllers once, when Edit is tapped, and never
// re-seeds them; Save then submits all eight text fields from those
// controllers. If a freshness reload moved the base to the remote row
// meanwhile, `desired != base` holds even though the user typed nothing —
// which is exactly the condition the repository's keep-theirs branch needs to
// be FALSE. A later save (adding an example clip, say) would then write the
// stale text back over the remote change under a fresh stamp.

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:provider/provider.dart';
import 'package:vocara/data/repositories/technique_repository.dart';
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/data/services/technique_media_service.dart';
import 'package:vocara/data/sync/sync_engine.dart';
import 'package:vocara/data/sync/sync_hashes.dart';
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/ui/technique_library/view_model/technique_detail_view_model.dart';
import 'package:vocara/ui/technique_library/widgets/technique_detail_screen.dart';
import 'package:vocara/utils/result.dart';

/// Serves whatever `stored`/`storedHashes` currently hold, and records the base
/// each save was merged against — the value the whole defect turns on.
class _Repo implements TechniqueRepository {
  _Repo(this.stored, this.storedHashes);

  Technique stored;
  Map<String, String> storedHashes;
  final savedWithBase = <Map<String, String>>[];
  final savedTechniques = <Technique>[];
  int loadCalls = 0;

  @override
  Future<Result<Technique>> getTechnique(String id) async => Result.ok(stored);

  @override
  Future<Result<Loaded<Technique>>> loadTechnique(String id) async {
    loadCalls++;
    return Result.ok(Loaded(stored, storedHashes));
  }

  @override
  Future<Result<List<Technique>>> listTechniques() async => Result.ok([stored]);

  @override
  Future<Result<Technique>> saveTechnique(Technique t) async {
    stored = t;
    return Result.ok(t);
  }

  @override
  Future<Result<Loaded<Technique>>> saveTechniqueWithBase(
    Technique t, {
    required Map<String, String> baseHashes,
  }) async {
    savedWithBase.add(Map.of(baseHashes));
    savedTechniques.add(t);
    stored = t;
    return Result.ok(Loaded(t, storedHashes));
  }

  @override
  Future<Result<void>> deleteTechnique(String id) async =>
      const Result.ok(null);
}

Technique _technique({String summary = 'local summary'}) => Technique(
  id: 't1',
  name: 'Twang',
  summary: summary,
  description: 'A description.',
  cues: 'Quack like a duck.',
  createdAt: DateTime(2026, 6, 20),
  updatedAt: DateTime(2026, 6, 20),
);

Future<(TechniqueDetailViewModel, _Repo, StreamController<RemoteApplied>)>
_pump(WidgetTester tester) async {
  final repo = _Repo(_technique(), {'t1': 'BASE-HASH'});
  final remote = StreamController<RemoteApplied>.broadcast();
  final vm = TechniqueDetailViewModel(
    repository: repo,
    mediaService: TechniqueMediaService(storage: LocalStorageService()),
    techniqueId: 't1',
    remoteApplied: remote.stream,
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
  return (vm, repo, remote);
}

/// Stand in for sync applying a remote edit of this technique: the stored row
/// and its hash both move on.
void _remoteEdit(_Repo repo, StreamController<RemoteApplied> remote) {
  repo.stored = repo.stored.copyWith(summary: 'remote summary');
  repo.storedHashes = {'t1': 'REMOTE-HASH'};
  remote.add(
    RemoteApplied(rows: const [], practiceIds: const {}, techniqueIds: {'t1'}),
  );
}

void main() {
  testWidgets('a remote change mid-edit does not advance the save base', (
    tester,
  ) async {
    final (vm, repo, remote) = await _pump(tester);
    addTearDown(remote.close);

    await tester.tap(find.text('Edit').first);
    await tester.pumpAndSettle();

    _remoteEdit(repo, remote);
    await tester.pumpAndSettle();

    // The form's controllers still hold the pre-remote text, so the aggregate
    // and base they will be saved against must not have moved either.
    expect(vm.technique!.summary, 'local summary');

    await tester.tap(find.text('Save').first);
    await tester.pumpAndSettle();

    expect(repo.savedWithBase, hasLength(1));
    expect(
      repo.savedWithBase.single['t1'],
      'BASE-HASH',
      reason:
          'the save must merge against the snapshot the form was seeded '
          'from, or keep-theirs can never fire',
    );
  });

  testWidgets('a reload skipped during the edit is replayed on cancel', (
    tester,
  ) async {
    final (vm, repo, remote) = await _pump(tester);
    addTearDown(remote.close);

    await tester.tap(find.text('Edit').first);
    await tester.pumpAndSettle();
    _remoteEdit(repo, remote);
    await tester.pumpAndSettle();
    expect(vm.technique!.summary, 'local summary');

    // Leaving edit without saving: pinning the base was only ever about the
    // open form, so the read view must not be left showing stale content with
    // nothing pending to correct it.
    await tester.tap(find.text('Cancel').first);
    await tester.pumpAndSettle();
    expect(vm.technique!.summary, 'remote summary');
  });

  testWidgets('outside an edit session a remote change still refreshes', (
    tester,
  ) async {
    final (vm, repo, remote) = await _pump(tester);
    addTearDown(remote.close);

    _remoteEdit(repo, remote);
    await tester.pumpAndSettle();
    expect(vm.technique!.summary, 'remote summary');
  });
}
