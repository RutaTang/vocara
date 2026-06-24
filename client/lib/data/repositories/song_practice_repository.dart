import '../../domain/models/song_practice.dart';
import '../../domain/models/take_version.dart';
import '../../utils/id.dart';
import '../../utils/result.dart';
import '../services/local_storage_service.dart';

/// Single source of truth for [SongPractice] aggregates. Both the home list
/// and the detail view read and write through this repository. Returns
/// [Result] rather than throwing.
abstract class SongPracticeRepository {
  Future<Result<List<SongPractice>>> listPractices();
  Future<Result<SongPractice>> getPractice(String id);
  Future<Result<SongPractice>> createPractice(String title);

  /// Persists [practice] (bumping its `updatedAt`) and returns the saved copy.
  Future<Result<SongPractice>> savePractice(SongPractice practice);
  Future<Result<void>> deletePractice(String id);
}

/// Local, file-backed implementation persisting one JSON document per practice.
class SongPracticeRepositoryLocal implements SongPracticeRepository {
  SongPracticeRepositoryLocal({required LocalStorageService storage})
    : _storage = storage;

  final LocalStorageService _storage;

  static const _collection = 'practices';

  @override
  Future<Result<List<SongPractice>>> listPractices() async {
    try {
      final docs = await _storage.readAll(_collection);
      final practices = docs.map(SongPractice.fromJson).toList()
        ..sort((a, b) => b.updatedAt.compareTo(a.updatedAt));
      return Result.ok(practices);
    } on Exception catch (e) {
      return Result.error(e);
    }
  }

  @override
  Future<Result<SongPractice>> getPractice(String id) async {
    try {
      final doc = await _storage.read(_collection, id);
      if (doc == null) {
        return Result.error(Exception('Practice "$id" not found'));
      }
      return Result.ok(SongPractice.fromJson(doc));
    } on Exception catch (e) {
      return Result.error(e);
    }
  }

  @override
  Future<Result<SongPractice>> createPractice(String title) async {
    try {
      final now = DateTime.now();
      final trimmed = title.trim();
      // Every practice starts with one interpretation to record takes under.
      final firstVersion = TakeVersion(
        id: newId(),
        name: 'Interpretation 1',
        createdAt: now,
      );
      final practice = SongPractice(
        id: newId(),
        title: trimmed.isEmpty ? 'Untitled practice' : trimmed,
        versions: [firstVersion],
        activeVersionId: firstVersion.id,
        createdAt: now,
        updatedAt: now,
      );
      await _storage.write(_collection, practice.id, practice.toJson());
      return Result.ok(practice);
    } on Exception catch (e) {
      return Result.error(e);
    }
  }

  @override
  Future<Result<SongPractice>> savePractice(SongPractice practice) async {
    try {
      final updated = practice.copyWith(updatedAt: DateTime.now());
      await _storage.write(_collection, updated.id, updated.toJson());
      return Result.ok(updated);
    } on Exception catch (e) {
      return Result.error(e);
    }
  }

  @override
  Future<Result<void>> deletePractice(String id) async {
    try {
      await _storage.delete(_collection, id);
      return const Result.ok(null);
    } on Exception catch (e) {
      return Result.error(e);
    }
  }
}
