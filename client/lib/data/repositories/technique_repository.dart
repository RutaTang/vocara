import '../../domain/models/technique.dart';
import '../../utils/result.dart';
import '../sync/sync_hashes.dart';

/// Single source of truth for the technique library. The library is a FLAT pool
/// of [Technique] records organized only by their tags (there is no collection
/// container) — a technique can carry any number of tags, including a framework
/// tag like "EVT" or "CVT". Returns [Result] rather than throwing.
///
/// Editing flows MUST use the base-hash pair [loadTechnique] +
/// [saveTechniqueWithBase]: the base hashes captured at load make the save a
/// three-way merge, so a stale open technique can neither revert nor tombstone
/// rows that changed underneath it (e.g. applied by sync). The base-less
/// [saveTechnique] stays only for creation flows.
abstract class TechniqueRepository {
  Future<Result<List<Technique>>> listTechniques();

  Future<Result<Technique>> getTechnique(String id);

  /// Loads one technique FOR EDITING: the technique plus the per-row base
  /// content hashes of everything read (see [Loaded]). Hand the hashes back to
  /// [saveTechniqueWithBase] unchanged.
  Future<Result<Loaded<Technique>>> loadTechnique(String id);

  /// Persists [technique] (bumping its `updatedAt`) and returns the saved copy.
  ///
  /// NO base hashes: the save adopts the CURRENT persisted state as its base,
  /// which is the old two-way diff — correct only when the technique was just
  /// created or re-read immediately before saving. A long-lived editor holding
  /// the technique across other writes must use [saveTechniqueWithBase]
  /// instead, or its stale save can revert or delete concurrent changes.
  Future<Result<Technique>> saveTechnique(Technique technique);

  /// Persists [technique] as a THREE-WAY merge against [baseHashes] (captured
  /// by [loadTechnique] or returned by the previous save) and returns the
  /// saved technique re-read from the store together with FRESH base hashes
  /// for the next save — and, as [Loaded.sentHashes], the hashes of
  /// [technique] as sent.
  ///
  /// An error means no row of the technique was written; media it imported
  /// into the asset pool before the failure may remain (the pool's sweep
  /// reclaims what nothing references). A deleted technique is refused before
  /// any import. A write that committed but whose re-read failed still
  /// succeeds, with [technique] itself and its sent hashes standing in for the
  /// re-read ([Loaded.reread] false).
  Future<Result<Loaded<Technique>>> saveTechniqueWithBase(
    Technique technique, {
    required Map<String, String> baseHashes,
  });

  Future<Result<void>> deleteTechnique(String id);
}
