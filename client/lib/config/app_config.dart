/// Compile-time dev/demo toggles, set with `--dart-define`. Centralized here
/// so the screenshot/demo hooks are discoverable in one place instead of
/// scattered `fromEnvironment` lookups inside view models.
abstract final class AppConfig {
  /// Opens the analysis dock on launch (dock layout previews/screenshots).
  static const bool voiceprintDemo = bool.fromEnvironment('VOICEPRINT_DEMO');

  /// Shows the Live voiceprint layout on load (without starting the mic).
  static const bool liveDemo = bool.fromEnvironment('LIVE_DEMO');

  /// Opens a specific dock tab by `DockTab` name on load (empty = default).
  static const String dockTab = String.fromEnvironment('DOCK_TAB');

  /// Folds out every vocal take with all analysis lanes on load.
  static const bool expandPitch = bool.fromEnvironment('EXPAND_PITCH');
}
