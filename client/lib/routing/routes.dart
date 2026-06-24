/// Centralized route paths for the app.
abstract final class Routes {
  /// Home — the collection of song practices.
  static const home = '/';

  /// Route pattern for the song-practice detail view (`/song/:id`).
  static const songPracticePattern = '/song/:id';

  /// Builds the detail path for a given practice [id].
  static String songPractice(String id) => '/song/$id';

  /// Global app settings (audio devices).
  static const settings = '/settings';

  /// The technique library — a flat, tag-organized pool of vocal techniques.
  static const techniqueLibrary = '/techniques';

  /// Route pattern for a single technique's detail (`/techniques/:id`).
  static const techniqueDetailPattern = '/techniques/:id';

  /// Builds the detail path for a given technique [id].
  static String techniqueDetail(String id) => '/techniques/$id';
}
