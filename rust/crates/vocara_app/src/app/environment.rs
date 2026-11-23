//! The build environment (dev / test / prod), read from the `VOCARA_FLAVOR`
//! environment variable.
//!
//! LOCAL STAND-IN: this is the part of `config/environment.dart` the shell
//! needs (the window title, the drag bar's badge, the bundle id the library
//! lives under). `vocara_model::config::environment` is being ported in
//! parallel; once it lands, the shell uses that and this module goes.
//!
//! Dart resolved the flavor from Flutter's compile-time `appFlavor`; the Rust
//! app reads it at startup. Unknown or absent falls back to dev, and the
//! match is case-insensitive (a release build used to get the Xcode scheme
//! name, "Prod", and an exact match sent it down the dev fallback — which
//! also selects the Python-sidecar separation service a released build must
//! never run).

use std::sync::OnceLock;

/// The environment variable the flavor is read from.
pub const FLAVOR_VAR: &str = "VOCARA_FLAVOR";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppEnvironment {
    Dev,
    Test,
    Prod,
}

impl AppEnvironment {
    /// The process's environment, resolved once from `VOCARA_FLAVOR`.
    pub fn current() -> AppEnvironment {
        static CURRENT: OnceLock<AppEnvironment> = OnceLock::new();
        *CURRENT.get_or_init(|| Self::resolve(std::env::var(FLAVOR_VAR).ok().as_deref()))
    }

    /// CASE-INSENSITIVE on purpose (see the module docs).
    pub fn resolve(flavor: Option<&str>) -> AppEnvironment {
        match flavor.map(str::to_lowercase).as_deref() {
            Some("prod") => AppEnvironment::Prod,
            Some("test") => AppEnvironment::Test,
            _ => AppEnvironment::Dev,
        }
    }

    pub fn is_prod(self) -> bool {
        self == AppEnvironment::Prod
    }

    fn name(self) -> &'static str {
        match self {
            AppEnvironment::Dev => "dev",
            AppEnvironment::Test => "test",
            AppEnvironment::Prod => "prod",
        }
    }

    /// Short uppercase tag for the in-window badge ("DEV" / "TEST"); empty
    /// in prod.
    pub fn badge(self) -> String {
        if self.is_prod() {
            String::new()
        } else {
            self.name().to_uppercase()
        }
    }

    /// Window title — plain "Vocara" in prod, suffixed in dev/test so you can
    /// tell which environment a window belongs to in ⌘-Tab / Mission Control.
    pub fn window_title(self) -> String {
        if self.is_prod() {
            "Vocara".to_string()
        } else {
            format!("Vocara — {}", self.name().to_uppercase())
        }
    }

    /// The macOS bundle id each environment is (and the library's
    /// application-support folder name): `com.rutatang.vocara`, `.dev`,
    /// `.test`.
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
    use super::*;

    #[test]
    fn resolves_case_insensitively_and_falls_back_to_dev() {
        assert_eq!(AppEnvironment::resolve(Some("Prod")), AppEnvironment::Prod);
        assert_eq!(AppEnvironment::resolve(Some("TEST")), AppEnvironment::Test);
        assert_eq!(AppEnvironment::resolve(Some("dev")), AppEnvironment::Dev);
        assert_eq!(
            AppEnvironment::resolve(Some("staging")),
            AppEnvironment::Dev
        );
        assert_eq!(AppEnvironment::resolve(None), AppEnvironment::Dev);
    }

    #[test]
    fn titles_and_badges_name_the_environment_outside_prod() {
        assert_eq!(AppEnvironment::Prod.window_title(), "Vocara");
        assert_eq!(AppEnvironment::Dev.window_title(), "Vocara — DEV");
        assert_eq!(AppEnvironment::Test.badge(), "TEST");
        assert_eq!(AppEnvironment::Prod.badge(), "");
    }
}
