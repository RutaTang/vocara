//! The build environment (port of `lib/config/environment.dart`).

use std::sync::OnceLock;

/// The build environment. Flutter derived it from the `--flavor` value
/// (`appFlavor`); the Rust app reads it from the [`FLAVOR_VAR`] environment
/// variable once, at startup, with the same rules ([`AppEnvironment::resolve`]).
///
/// Each environment is a distinct macOS bundle id (`packaging/macos/bundle.sh`
/// picks it from the flavor):
///   dev  -> com.rutatang.vocara.dev
///   test -> com.rutatang.vocara.test
///   prod -> com.rutatang.vocara
/// The store location (the drift db + asset pool, and with it the HLC device
/// node) is keyed by bundle id: each one gets its own application support
/// folder, `~/Library/Application Support/<bundle id>`, so dev/test/prod
/// coexist on one machine without clobbering each other. No build is
/// sandboxed any more (`packaging/macos/Release.entitlements`), so debug and
/// release builds of one flavor share that folder. Releases up to 0.3.0 were
/// sandboxed and kept theirs in their App Sandbox container instead — while
/// debug bundles and `cargo run` used the plain folder, two disjoint stores
/// with two HLC node ids for one bundle id. The first launch of a later
/// release moves the container's library out
/// (`vocara_services::sandbox_migration`); where the plain folder already
/// holds a library of its own, that one opens and the container's is left
/// as it is.
///
/// Builds are flavor-only; an unflavored build (no flavor) is not a normal
/// path but falls back to [`AppEnvironment::Dev`] for safety.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AppEnvironment {
    Dev,
    Test,
    Prod,
}

/// The environment variable the flavor is read from.
pub const FLAVOR_VAR: &str = "VOCARA_FLAVOR";

impl AppEnvironment {
    /// Resolved once (the Flutter app injected `appFlavor` from the
    /// `--flavor` argument): the flavor the binary was BUILT with
    /// ([`FLAVOR_VAR`] at compile time, which `packaging/macos/bundle.sh`
    /// sets, since an app launched from Finder has no environment of its own),
    /// unless [`FLAVOR_VAR`] is set when it runs (development and tests).
    /// Unknown or absent flavor falls back to [`AppEnvironment::Dev`].
    pub fn current() -> AppEnvironment {
        static CURRENT: OnceLock<AppEnvironment> = OnceLock::new();
        *CURRENT.get_or_init(|| {
            let runtime = std::env::var(FLAVOR_VAR).ok();
            AppEnvironment::resolve(runtime.as_deref().or(option_env!("VOCARA_FLAVOR")))
        })
    }

    /// CASE-INSENSITIVE on purpose: `flutter build macos --flavor prod` injects
    /// the Xcode SCHEME name ("Prod", capitalized), not the lowercase flavor
    /// argument, so an exact match sent every release build down the dev
    /// fallback. The badge was the visible symptom; the real harm is that dev
    /// mode also selects the Python-sidecar separation service, which a
    /// released build must never run (see createSourceSeparationService).
    pub fn resolve(flavor: Option<&str>) -> AppEnvironment {
        match flavor.map(str::to_lowercase).as_deref() {
            Some("prod") => AppEnvironment::Prod,
            Some("test") => AppEnvironment::Test,
            _ => AppEnvironment::Dev,
        }
    }

    /// The Dart enum name: `dev`, `test` or `prod`.
    pub fn name(self) -> &'static str {
        match self {
            AppEnvironment::Dev => "dev",
            AppEnvironment::Test => "test",
            AppEnvironment::Prod => "prod",
        }
    }

    pub fn is_prod(self) -> bool {
        self == AppEnvironment::Prod
    }

    /// Short uppercase tag for the in-window badge ("DEV" / "TEST"); empty in prod.
    pub fn badge(self) -> String {
        if self.is_prod() {
            String::new()
        } else {
            self.name().to_uppercase()
        }
    }

    /// Window title — plain "Vocara" in prod, suffixed in dev/test so you can tell
    /// which environment a window belongs to in ⌘-Tab / Mission Control.
    pub fn window_title(self) -> String {
        if self.is_prod() {
            "Vocara".to_string()
        } else {
            format!("Vocara — {}", self.name().to_uppercase())
        }
    }

    /// The macOS bundle id of this environment's app (see the type doc): the
    /// name of its application support directory.
    pub fn bundle_id(self) -> &'static str {
        match self {
            AppEnvironment::Dev => "com.rutatang.vocara.dev",
            AppEnvironment::Test => "com.rutatang.vocara.test",
            AppEnvironment::Prod => "com.rutatang.vocara",
        }
    }
}

#[cfg(test)]
mod tests {
    //! Port of `client/test/environment_test.dart`. The flavor string arrives
    //! in whatever casing the build pipeline uses — `flutter build macos
    //! --flavor prod` injects the Xcode SCHEME name ("Prod"). The v0.1.0
    //! release shipped as dev because resolution was case-sensitive; these
    //! tests pin the case-insensitive contract.
    use super::AppEnvironment::{self, *};

    #[test]
    fn scheme_cased_flavors_resolve_to_their_environments() {
        assert_eq!(AppEnvironment::resolve(Some("Prod")), Prod);
        assert_eq!(AppEnvironment::resolve(Some("Test")), Test);
        assert_eq!(AppEnvironment::resolve(Some("Dev")), Dev);
    }

    #[test]
    fn lowercase_flavor_arguments_resolve_identically() {
        assert_eq!(AppEnvironment::resolve(Some("prod")), Prod);
        assert_eq!(AppEnvironment::resolve(Some("test")), Test);
        assert_eq!(AppEnvironment::resolve(Some("dev")), Dev);
    }

    #[test]
    fn absent_or_unknown_flavor_falls_back_to_dev_never_prod() {
        assert_eq!(AppEnvironment::resolve(None), Dev);
        assert_eq!(AppEnvironment::resolve(Some("")), Dev);
        assert_eq!(AppEnvironment::resolve(Some("staging")), Dev);
    }

    #[test]
    fn prod_hides_the_badge_and_uses_the_plain_window_title() {
        assert!(Prod.badge().is_empty());
        assert_eq!(Prod.window_title(), "Vocara");
        assert_eq!(Dev.badge(), "DEV");
        assert_eq!(Test.badge(), "TEST");
        assert_eq!(Test.window_title(), "Vocara — TEST");
    }
}
