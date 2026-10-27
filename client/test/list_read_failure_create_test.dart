// Home and the technique library show their read error only over an empty
// list. A create after a failed read added its card to the list never read:
// the error gave way to that one card, every later read failed too, and the
// rest of the library looked lost.

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/repositories/song_practice_repository.dart';
import 'package:vocara/data/repositories/technique_repository.dart';
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/data/services/technique_media_service.dart';
import 'package:vocara/domain/models/practice_summary.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/ui/home/view_model/home_view_model.dart';
import 'package:vocara/ui/technique_library/view_model/technique_library_view_model.dart';
import 'package:vocara/utils/result.dart';

final _at = DateTime(2026, 6, 20);

class _Practices extends Fake implements SongPracticeRepository {
  bool readable = false;
  final stored = [
    PracticeSummary(
      id: 'P1',
      title: 'Kept',
      updatedAt: _at,
      takeCount: 0,
      hasAccompaniment: false,
      noteCount: 0,
    ),
  ];

  @override
  Future<Result<List<PracticeSummary>>> listPracticeSummaries() async =>
      readable ? Result.ok([...stored]) : Result.error(Exception('read'));

  @override
  Future<Result<SongPractice>> createPractice(String title) async {
    final p = SongPractice(
      id: 'P2',
      title: title,
      createdAt: _at,
      updatedAt: _at,
    );
    stored.insert(0, PracticeSummary.of(p));
    return Result.ok(p);
  }
}

class _Techniques extends Fake implements TechniqueRepository {
  bool readable = false;
  final stored = [
    Technique(id: 'T1', name: 'Kept', createdAt: _at, updatedAt: _at),
  ];

  @override
  Future<Result<List<Technique>>> listTechniques() async =>
      readable ? Result.ok([...stored]) : Result.error(Exception('read'));

  @override
  Future<Result<Technique>> saveTechnique(Technique t) async {
    stored.insert(0, t);
    return Result.ok(t);
  }
}

Future<void> _settled(bool Function() running) async {
  for (var i = 0; i < 100 && running(); i++) {
    await pumpEventQueue();
  }
}

void main() {
  test('Home keeps its read error over a practice created after it', () async {
    final repo = _Practices();
    final vm = HomeViewModel(repository: repo);
    addTearDown(vm.dispose);
    await _settled(() => vm.load.running);
    expect(vm.load.error, isTrue);

    await vm.createProject.execute('New');
    expect(vm.createProject.result, isA<Ok<SongPractice>>());
    expect(vm.projects, isEmpty, reason: 'the error still shows');

    repo.readable = true;
    vm.refresh();
    await _settled(() => vm.load.running);
    expect(vm.projects.map((p) => p.title), ['New', 'Kept']);
  });

  test('the library keeps its read error over a technique created after '
      'it', () async {
    final repo = _Techniques();
    final vm = TechniqueLibraryViewModel(
      repository: repo,
      mediaService: TechniqueMediaService(storage: LocalStorageService()),
    );
    addTearDown(vm.dispose);
    await _settled(() => vm.load.running);
    expect(vm.load.error, isTrue);

    await vm.createTechnique.execute('New');
    expect(vm.createTechnique.result, isA<Ok<Technique>>());
    expect(vm.all, isEmpty, reason: 'the error still shows');

    repo.readable = true;
    vm.refresh();
    await _settled(() => vm.load.running);
    expect(vm.all.map((t) => t.name), ['New', 'Kept']);
  });
}
