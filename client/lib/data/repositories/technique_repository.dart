import '../../domain/models/technique.dart';
import '../../utils/result.dart';

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
