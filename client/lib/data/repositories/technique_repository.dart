import '../../domain/models/technique.dart';
import '../../utils/result.dart';
import '../services/local_storage_service.dart';

/// Single source of truth for the technique library. The library is a FLAT pool
/// of [Technique] records organized only by their tags (there is no collection
/// container) — a technique can carry any number of tags, including a framework
/// tag like "EVT" or "CVT". Returns [Result] rather than throwing.
abstract class TechniqueRepository {
  Future<Result<List<Technique>>> listTechniques();
  Future<Result<Technique>> getTechnique(String id);

  /// Persists [technique] (bumping its `updatedAt`) and returns the saved copy.
  /// Used for both create and every edit — the file is keyed by id.
  Future<Result<Technique>> saveTechnique(Technique technique);

  Future<Result<void>> deleteTechnique(String id);
}

/// Local, file-backed implementation persisting one JSON document per technique.
class TechniqueRepositoryLocal implements TechniqueRepository {
  TechniqueRepositoryLocal({required LocalStorageService storage})
    : _storage = storage;

  final LocalStorageService _storage;

  static const _collection = 'techniques';

  @override
  Future<Result<List<Technique>>> listTechniques() async {
    try {
      final docs = await _storage.readAll(_collection);
      final techniques = docs.map(Technique.fromJson).toList()
        ..sort((a, b) => b.updatedAt.compareTo(a.updatedAt));
      return Result.ok(techniques);
    } on Exception catch (e) {
      return Result.error(e);
    }
  }

  @override
  Future<Result<Technique>> getTechnique(String id) async {
    try {
      final doc = await _storage.read(_collection, id);
      if (doc == null) {
        return Result.error(Exception('Technique "$id" not found'));
      }
      return Result.ok(Technique.fromJson(doc));
    } on Exception catch (e) {
      return Result.error(e);
    }
  }

  @override
  Future<Result<Technique>> saveTechnique(Technique technique) async {
    try {
      final updated = technique.copyWith(updatedAt: DateTime.now());
      await _storage.write(_collection, updated.id, updated.toJson());
      return Result.ok(updated);
    } on Exception catch (e) {
      return Result.error(e);
    }
  }

  @override
  Future<Result<void>> deleteTechnique(String id) async {
    try {
      await _storage.delete(_collection, id);
      return const Result.ok(null);
    } on Exception catch (e) {
      return Result.error(e);
    }
  }
}
