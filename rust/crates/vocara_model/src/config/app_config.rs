//! Build-time dev/demo toggles (port of `lib/config/app_config.dart`).
//!
//! Dart set these with `--dart-define`, read at compile time with
//! `bool.fromEnvironment` / `String.fromEnvironment`. The Rust counterparts
//! are read at compile time too, from the same-named environment variables
//! of the build (`VOICEPRINT_DEMO=true cargo build …`). As in Dart, a flag is
//! on only when its value is exactly `true`.

/// `bool.fromEnvironment`: exactly `"true"`.
const fn is_true(value: Option<&str>) -> bool {
    match value {
        Some(s) => {
            let b = s.as_bytes();
            b.len() == 4 && b[0] == b't' && b[1] == b'r' && b[2] == b'u' && b[3] == b'e'
        }
        None => false,
    }
}

/// `String.fromEnvironment`: the value, or empty.
const fn or_empty(value: Option<&'static str>) -> &'static str {
    match value {
        Some(s) => s,
        None => "",
    }
}

/// Compile-time dev/demo toggles. Centralized here so the screenshot/demo
/// hooks are discoverable in one place instead of scattered lookups inside
/// view models.
pub struct AppConfig;

impl AppConfig {
    /// Opens the analysis dock on launch (dock layout previews/screenshots).
    pub const VOICEPRINT_DEMO: bool = is_true(option_env!("VOICEPRINT_DEMO"));

    /// Shows the Live voiceprint layout on load (without starting the mic).
    pub const LIVE_DEMO: bool = is_true(option_env!("LIVE_DEMO"));

    /// Opens a specific dock tab by `DockTab` name on load (empty = default).
    pub const DOCK_TAB: &'static str = or_empty(option_env!("DOCK_TAB"));

    /// Folds out every vocal take with all analysis lanes on load.
    pub const EXPAND_PITCH: bool = is_true(option_env!("EXPAND_PITCH"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flag_is_on_only_for_exactly_true() {
        assert!(is_true(Some("true")));
        assert!(!is_true(Some("TRUE")));
        assert!(!is_true(Some("1")));
        assert!(!is_true(None));
        assert_eq!(or_empty(None), "");
        assert_eq!(or_empty(Some("live")), "live");
    }
}
