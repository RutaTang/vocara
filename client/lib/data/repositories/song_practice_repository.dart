import '../../domain/models/practice_summary.dart';
import '../../domain/models/song_practice.dart';
import '../../utils/result.dart';
import '../sync/sync_hashes.dart';

/// Single source of truth for [SongPractice] aggregates. Both the home list
/// and the detail view read and write through this repository. Returns
/// [Result] rather than throwing.
///
/// Editing flows MUST use the base-hash pair [loadPractice] +
/// [savePracticeWithBase]: the base hashes captured at load make the save a
/// three-way merge, so a stale open aggregate can neither revert nor tombstone
/// rows that changed underneath it (e.g. applied by sync). The base-less
/// [savePractice] stays only for create/append flows that just built or
/// re-read the aggregate.
abstract class SongPracticeRepository {
  Future<Result<List<SongPractice>>> listPractices();

  /// What the Home list shows of every live practice (see
  /// [PracticeSummary]), newest first like [listPractices] — without
  /// hydrating any of them. So unlike [listPractices], a practice whose rows
  /// this build cannot read (a malformed or newer-format JSON column) is
  /// still listed: Home can delete it, and opening it says it could not be
  /// loaded, where before it simply vanished from the list.
  Future<Result<List<PracticeSummary>>> listPracticeSummaries();

  Future<Result<SongPractice>> getPractice(String id);

  /// Loads one aggregate FOR EDITING: the practice plus the per-row base
  /// content hashes of everything read (see [Loaded]). Hand the hashes back to
  /// [savePracticeWithBase] unchanged.
  Future<Result<Loaded<SongPractice>>> loadPractice(String id);

  Future<Result<SongPractice>> createPractice(String title);

  /// Persists [practice] (bumping its `updatedAt`) and returns the saved copy.
  ///
  /// NO base hashes: the save adopts the CURRENT persisted state as its base,
  /// which is the old two-way diff — correct only when the aggregate was just
  /// created or re-read immediately before saving (create/append/recovery
  /// flows). A long-lived editor holding the aggregate across other writes
  /// must use [savePracticeWithBase] instead, or its stale save can revert or
  /// delete concurrent changes.
  Future<Result<SongPractice>> savePractice(SongPractice practice);

  /// Persists [practice] as a THREE-WAY merge against [baseHashes] (captured
  /// by [loadPractice] or returned by the previous save) and returns the saved
  /// aggregate re-read from the store together with FRESH base hashes for the
  /// next save — and, as [Loaded.sentHashes], the hashes of [practice] as
  /// sent: the base for a caller that kept a model newer than [practice].
  Future<Result<Loaded<SongPractice>>> savePracticeWithBase(
    SongPractice practice, {
    required Map<String, String> baseHashes,
  });

  Future<Result<void>> deletePractice(String id);
}
