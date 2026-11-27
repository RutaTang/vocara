//! The version rule an update obeys: never backwards, never sideways, and
//! exactly the release the user agreed to (Clew's `update::ensure_upgrade`
//! and `install::check_candidate_version`), over the order the update check
//! already uses (`UpdateCheckService::is_newer`, proved a strict order in
//! `verification/VocaraVerify/Version.lean`).
//!
//! The rule is applied twice. A release is OFFERED only when its manifest's
//! version is newer than the running one (the update check). And the
//! installer applies it again to the version the downloaded BUNDLE declares
//! (its `CFBundleShortVersionString`, read after its signature was checked,
//! which seals it): a manifest's version is a label on a web page and says
//! nothing about which build the image holds, and every older notarized
//! Vocara passes the signature check, so without this any of them could be
//! served as an "update" and roll the user back.

use vocara_model::diagnostics::DiagnosticsLog;

use crate::update_check_service::{normalize_version, parse_version, UpdateCheckService};

/// The no-downgrade check: `candidate` (the version the downloaded bundle
/// declares) must read as a version and be strictly newer than `running`.
/// Its normalized form when it is.
pub fn ensure_upgrade(running: &str, candidate: &str) -> Result<String, String> {
    if parse_version(candidate).is_none() {
        return Err(format!(
            "the update declares an unreadable version {candidate:?}"
        ));
    }
    let candidate = normalize_version(candidate);
    if !UpdateCheckService::is_newer(&candidate, running) {
        return Err(format!(
            "the update contains Vocara {candidate}, which is not newer than the running \
             {running}, so it was not installed"
        ));
    }
    Ok(candidate)
}

/// The installer's version gate: the verified bundle's own version must be
/// newer than what is running ([`ensure_upgrade`]) and exactly the release
/// the user was offered (`expected`).
pub fn check_candidate_version(
    candidate: &str,
    expected: &str,
    running: &str,
) -> Result<(), String> {
    let candidate = ensure_upgrade(running, candidate)?;
    if !UpdateCheckService::same_version(&candidate, expected) {
        return Err(format!(
            "the update image contains Vocara {candidate}, but {expected} was offered, so \
             Vocara won't install it"
        ));
    }
    Ok(())
}

/// [`super::current_version`]'s rule: the bundle's version when it declares
/// a usable one, else the version the binary was built as.
pub(crate) fn running_version(bundle: Option<&str>, built: &str) -> String {
    let Some(declared) = bundle else {
        return built.to_string();
    };
    if parse_version(declared).is_none() {
        DiagnosticsLog::instance().log(
            "update",
            &format!("the app bundle's version {declared:?} is not a version; using {built}"),
        );
        return built.to_string();
    }
    let declared = normalize_version(declared);
    if !UpdateCheckService::same_version(&declared, built) {
        DiagnosticsLog::instance().log(
            "update",
            &format!(
                "this app bundle is version {declared} but was built as {built}; updates are \
                 compared against {declared}"
            ),
        );
    }
    declared
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Never backwards, and never sideways: the bundle's own version must be
    /// strictly newer than what is running.
    #[test]
    fn downgrades_and_reinstalls_are_refused() {
        assert_eq!(ensure_upgrade("1.4.2", "1.4.3").as_deref(), Ok("1.4.3"));
        assert_eq!(ensure_upgrade("1.4.2", "v2.0.0").as_deref(), Ok("2.0.0"));
        assert!(ensure_upgrade("1.4.2", "1.4.2").is_err(), "same version");
        assert!(
            ensure_upgrade("1.4.2", "1.4.2.0").is_err(),
            "the same, spelled longer"
        );
        let err = ensure_upgrade("1.4.2", "1.3.9").unwrap_err();
        assert!(err.contains("not newer"), "{err}");
        let err = ensure_upgrade("1.4.2", "garbage").unwrap_err();
        assert!(err.contains("unreadable"), "{err}");
        // A prerelease bundle is never an update.
        assert!(ensure_upgrade("1.4.2", "1.5.0-rc.1").is_err());
        // A prerelease build is offered its own release.
        assert!(ensure_upgrade("1.5.0-rc.1", "1.5.0").is_ok());
    }

    /// Provenance is not freshness: every older notarized Vocara passes the
    /// signature check, so the version is checked on its own.
    #[test]
    fn only_the_offered_newer_version_is_accepted() {
        let running = "0.3.0";
        assert!(check_candidate_version("0.4.0", "0.4.0", running).is_ok());
        assert!(check_candidate_version("0.4", "0.4.0", running).is_ok());
        let err = check_candidate_version("0.2.9", "0.4.0", running).unwrap_err();
        assert!(err.contains("not newer"), "{err}");
        assert!(check_candidate_version("0.3.0", "0.3.0", running).is_err());
        let err = check_candidate_version("0.5.0", "0.4.0", running).unwrap_err();
        assert!(err.contains("was offered"), "{err}");
        assert!(check_candidate_version("not a version", "0.4.0", running).is_err());
    }

    /// The running version is the bundle's when there is a usable one — what
    /// an update's own bundle version is compared with — and the built one
    /// otherwise.
    #[test]
    fn the_running_version_is_the_bundles_else_the_builds() {
        assert_eq!(running_version(Some("1.2.3"), "0.1.0"), "1.2.3");
        assert_eq!(running_version(Some("v0.3.0"), "0.3.0"), "0.3.0");
        assert_eq!(running_version(None, "0.3.0"), "0.3.0");
        assert_eq!(running_version(Some("not a version"), "0.3.0"), "0.3.0");
        // The tests run from no bundle.
        assert_eq!(
            super::super::current_version(),
            UpdateCheckService::APP_VERSION
        );
    }
}
