import 'package:flutter/services.dart' show appFlavor;

/// The build environment, derived from the Flutter `--flavor` value.
///
/// Each environment is a distinct macOS bundle id (see the flavor schemes and
/// `macos/Runner/Configs/{Dev,Test,Prod}.xcconfig`):
///   dev  -> com.rutatang.vocara.dev
///   test -> com.rutatang.vocara.test
///   prod -> com.rutatang.vocara
/// Because the macOS sandbox container (the drift db + asset pool) and the HLC
/// device node are keyed by bundle id, this gives each environment its OWN
/// store, so dev/test/prod coexist on one machine without clobbering each other.
///
/// Builds are flavor-only; an unflavored build (no `--flavor`) is not a normal
/// path but falls back to [dev] for safety.
enum AppEnvironment {
  dev,
  test,
  prod;

  /// Resolved once from the compile-time [appFlavor] (Flutter injects this from
  /// the `--flavor` argument). Unknown or absent flavor falls back to [dev].
  static final AppEnvironment current = _resolve(appFlavor);

  static AppEnvironment _resolve(String? flavor) {
    switch (flavor) {
      case 'prod':
        return AppEnvironment.prod;
      case 'test':
        return AppEnvironment.test;
      case 'dev':
      default:
        return AppEnvironment.dev;
    }
  }

  bool get isProd => this == AppEnvironment.prod;

  /// Short uppercase tag for the in-window badge ("DEV" / "TEST"); empty in prod.
  String get badge => isProd ? '' : name.toUpperCase();

  /// Window title — plain "Vocara" in prod, suffixed in dev/test so you can tell
  /// which environment a window belongs to in ⌘-Tab / Mission Control.
  String get windowTitle =>
      isProd ? 'Vocara' : 'Vocara — ${name.toUpperCase()}';
}
