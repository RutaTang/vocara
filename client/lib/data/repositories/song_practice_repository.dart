import '../../domain/models/song_practice.dart';
import '../../utils/result.dart';

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
