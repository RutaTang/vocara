import 'package:flutter/services.dart' show appFlavor;

/// The build environment, derived from the Flutter `--flavor` value.
///
/// Each environment is a distinct macOS bundle id (see the flavor schemes and
/// `macos/Runner/Configs/{Dev,Test,Prod}.xcconfig`):
///   dev  -> com.rutatang.vocara.dev
///   test -> com.rutatang.vocara.test
///   prod -> com.rutatang.vocara
/// The store location (the drift db + asset pool, and with it the HLC device
/// node) is keyed by bundle id AND build config. Each bundle id gets its own
/// macOS sandbox container, so dev/test/prod coexist on one machine without
/// clobbering each other. But Debug/Profile builds disable the sandbox (see
/// `macos/Runner/DebugProfile.entitlements`; deliberate dev-only choice so the
/// prototype Python separation sidecar can be spawned), so the SAME bundle id
/// resolves to the plain `~/Library/Application Support` path in Debug and to
/// the sandbox container in Release: two disjoint stores with two HLC node ids.
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
