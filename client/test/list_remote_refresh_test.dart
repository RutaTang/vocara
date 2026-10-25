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

void main() {
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
