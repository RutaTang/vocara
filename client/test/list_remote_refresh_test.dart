// Home's practice cards and the technique library's cards show counts that
// live in CHILD rows (takes and notes, example clips). A remote page that
// carries only those rows — another device recording a take, adding an
// example — must refresh the lists too, not just one touching the parents.

import 'dart:async';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/repositories/song_practice_repository.dart';
import 'package:vocara/data/repositories/technique_repository.dart';
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/data/services/technique_media_service.dart';
import 'package:vocara/data/sync/sync_engine.dart';
import 'package:vocara/data/sync/sync_hashes.dart';
import 'package:vocara/domain/models/practice_summary.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/ui/home/view_model/home_view_model.dart';
import 'package:vocara/ui/technique_library/view_model/technique_library_view_model.dart';
import 'package:vocara/utils/result.dart';

class _PracticeRepo implements SongPracticeRepository {
  int summaryReads = 0;

  @override
  Future<Result<List<SongPractice>>> listPractices() async =>
      const Result.ok([]);

  @override
  Future<Result<List<PracticeSummary>>> listPracticeSummaries() async {
    summaryReads++;
    return const Result.ok([]);
  }

  @override
  Future<Result<SongPractice>> getPractice(String id) async =>
      Result.error(Exception('not found'));

  @override
  Future<Result<Loaded<SongPractice>>> loadPractice(String id) async =>
      Result.error(Exception('not found'));

  @override
  Future<Result<SongPractice>> createPractice(String title) async =>
      Result.error(Exception('unsupported'));

  @override
  Future<Result<SongPractice>> savePractice(SongPractice practice) async =>
      Result.ok(practice);

  @override
  Future<Result<Loaded<SongPractice>>> savePracticeWithBase(
    SongPractice practice, {
    required Map<String, String> baseHashes,
  }) async => Result.ok(Loaded(practice, const {}));

  @override
  Future<Result<void>> deletePractice(String id) async => const Result.ok(null);
}

class _TechniqueRepo implements TechniqueRepository {
  int listReads = 0;

  @override
  Future<Result<List<Technique>>> listTechniques() async {
    listReads++;
    return const Result.ok([]);
  }

  @override
  Future<Result<Technique>> getTechnique(String id) async =>
      Result.error(Exception('not found'));

  @override
  Future<Result<Loaded<Technique>>> loadTechnique(String id) async =>
      Result.error(Exception('not found'));

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

/// Deletes no files.
class _NoDiskMedia extends TechniqueMediaService {
  _NoDiskMedia() : super(storage: LocalStorageService());

  @override
  Future<void> deleteTechniqueMedia(String techniqueId) async {}
}

/// A library whose list read can be held open ([gate]), over a list a test
/// edits as another device or this one would.
class _HeldTechniqueRepo extends _TechniqueRepo {
  List<Technique> techniques = [];
  Completer<void>? gate;

  @override
  Future<Result<List<Technique>>> listTechniques() async {
    listReads++;
    final read = List.of(techniques); // read now, returned when let go
    final g = gate;
    if (g != null) await g.future;
    return Result.ok(read);
  }

  @override
  Future<Result<void>> deleteTechnique(String id) async {
    techniques = [
      for (final t in techniques)
        if (t.id != id) t,
    ];
    return const Result.ok(null);
  }
}

Technique _technique(String id, String name) => Technique(
  id: id,
  name: name,
  createdAt: DateTime(2026),
  updatedAt: DateTime(2026),
);

void main() {
  test('a remote change landing during a load is loaded after it', () async {
    // Skipped while any command ran and forgotten: the card kept its old name
    // until some later reload.
    final repo = _HeldTechniqueRepo()..techniques = [_technique('a', 'Twang')];
    final remote = StreamController<RemoteApplied>.broadcast();
    addTearDown(remote.close);
    repo.gate = Completer<void>();
    final vm = TechniqueLibraryViewModel(
      repository: repo,
      mediaService: TechniqueMediaService(storage: LocalStorageService()),
      remoteApplied: remote.stream,
    );
    addTearDown(vm.dispose);
    await pumpEventQueue();
    expect(vm.load.running, isTrue);
    repo.techniques = [_technique('a', 'Twang (renamed)')];
    remote.add(
      RemoteApplied(
        rows: const [],
        practiceIds: const {},
        techniqueIds: const {'a'},
      ),
    );
    final first = repo.gate!;
    repo.gate = null;
    first.complete();
    await pumpEventQueue();
    expect(vm.all.map((t) => t.name), ['Twang (renamed)']);
  });

  test('a load read before a delete does not bring the card back', () async {
    final repo = _HeldTechniqueRepo()
      ..techniques = [_technique('a', 'A'), _technique('b', 'B')];
    final remote = StreamController<RemoteApplied>.broadcast();
    addTearDown(remote.close);
    final vm = TechniqueLibraryViewModel(
      repository: repo,
      mediaService: _NoDiskMedia(),
      remoteApplied: remote.stream,
    );
    addTearDown(vm.dispose);
    await pumpEventQueue();
    expect(vm.all.map((t) => t.id), ['a', 'b']);
    // A remote event's reload reads the list, and is held there.
    repo.gate = Completer<void>();
    remote.add(
      RemoteApplied(
        rows: const [],
        practiceIds: const {},
        techniqueIds: const {'b'},
      ),
    );
    await pumpEventQueue();
    expect(vm.load.running, isTrue);
    await vm.deleteTechnique.execute('a');
    expect(vm.all.map((t) => t.id), ['b']);
    final held = repo.gate!;
    repo.gate = null;
    held.complete(); // the older read lands
    await pumpEventQueue();
    expect(vm.all.map((t) => t.id), ['b']);
  });

  test('a return from a technique page while a load runs loads again after '
      'it', () async {
    // The return's load was dropped as re-entry, and the running one had read
    // the list before the delete made on that page: the card stayed.
    final repo = _HeldTechniqueRepo()
      ..techniques = [_technique('a', 'A'), _technique('b', 'B')];
    repo.gate = Completer<void>();
    final vm = TechniqueLibraryViewModel(
      repository: repo,
      mediaService: _NoDiskMedia(),
    );
    addTearDown(vm.dispose);
    await pumpEventQueue();
    expect(vm.load.running, isTrue);
    repo.techniques = [_technique('b', 'B')]; // deleted on its page
    vm.refresh();
    final held = repo.gate!;
    repo.gate = null;
    held.complete();
    await pumpEventQueue();
    expect(vm.all.map((t) => t.id), ['b']);
  });

  test('Home reloads when a remote page carries only a practice\'s takes '
      'and notes', () async {
    // Regression: Home filtered on the practices TABLE, and a take recorded
    // on another device ships only tracks/clips/notes rows — the card's take
    // and note counts never refreshed.
    final repo = _PracticeRepo();
    final remote = StreamController<RemoteApplied>.broadcast();
    addTearDown(remote.close);
    final vm = HomeViewModel(repository: repo, remoteApplied: remote.stream);
    addTearDown(vm.dispose);
    await pumpEventQueue();
    expect(repo.summaryReads, 1);

    remote.add(
      RemoteApplied(
        rows: const [('tracks', 'TR1'), ('practice_notes', 'N1')],
        practiceIds: const {'P1'},
        techniqueIds: const {},
      ),
    );
    await pumpEventQueue();
    expect(repo.summaryReads, 2);

    // A page touching no practice at all still leaves Home alone.
    remote.add(
      RemoteApplied(
        rows: const [('technique_media', 'M1')],
        practiceIds: const {},
        techniqueIds: const {'T1'},
      ),
    );
    await pumpEventQueue();
    expect(repo.summaryReads, 2);
  });

  test('the library reloads when a remote page carries only a technique\'s '
      'examples', () async {
    final repo = _TechniqueRepo();
    final remote = StreamController<RemoteApplied>.broadcast();
    addTearDown(remote.close);
    final vm = TechniqueLibraryViewModel(
      repository: repo,
      mediaService: TechniqueMediaService(storage: LocalStorageService()),
      remoteApplied: remote.stream,
    );
    addTearDown(vm.dispose);
    await pumpEventQueue();
    expect(repo.listReads, 1);

    remote.add(
      RemoteApplied(
        rows: const [('technique_media', 'M1')],
        practiceIds: const {},
        techniqueIds: const {'T1'},
      ),
    );
    await pumpEventQueue();
    expect(repo.listReads, 2);

    remote.add(
      RemoteApplied(
        rows: const [('tracks', 'TR1')],
        practiceIds: const {'P1'},
        techniqueIds: const {},
      ),
    );
    await pumpEventQueue();
    expect(repo.listReads, 2);
  });
}
