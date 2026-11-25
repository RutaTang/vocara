//! The update check's constants are pinned to the files they must agree with,
//! and its version comparison to the verified Lean model.

use vocara_services::update_check_service::UpdateCheckService;

fn repo_file(rel: &str) -> String {
    let path = format!("{}/../../../{rel}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// Port of `update_check_version_sync_test.dart`: a release with a stale
/// constant would ship an app that mis-identifies its own version and shows a
/// permanent "update available" banner for its own release. The workspace
/// version is what the release tag is checked against (release.yml) and what
/// the bundle's Info.plist carries (packaging/macos/bundle.sh).
#[test]
fn app_version_matches_the_workspace_version() {
    assert_eq!(UpdateCheckService::APP_VERSION, env!("CARGO_PKG_VERSION"));
    assert!(
        UpdateCheckService::APP_BUILD_NUMBER.parse::<u32>().is_ok(),
        "the build number is CFBundleVersion: a plain integer"
    );
}

/// Port of `update_check_manifest_sync_test.dart`: a repo renamed in one place
/// only would ship apps that ask a repo the workflow never publishes to.
#[test]
fn manifest_url_names_the_manifest_release_yml_publishes() {
    let workflow = repo_file(".github/workflows/release.yml");
    let repo = workflow
        .lines()
        .find_map(|l| l.trim().strip_prefix("RELEASES_REPO:"))
        .map(str::trim)
        .expect("release.yml must set RELEASES_REPO");
    assert_eq!(
        UpdateCheckService::MANIFEST_URL,
        format!("https://github.com/{repo}/releases/latest/download/latest.json")
    );
    assert!(workflow.contains("latest.json"));
}

/// The manual setup checks (`release-setup-check.yml`, and CI's
/// `release_setup_only` run) check the repo the release publishes to, and
/// all run the same check.
#[test]
fn the_setup_check_checks_the_repo_release_yml_publishes_to() {
    let releases_repo = |path: &str| {
        repo_file(path)
            .lines()
            .find_map(|l| {
                l.trim()
                    .strip_prefix("RELEASES_REPO:")
                    .map(|r| r.trim().to_string())
            })
            .unwrap_or_else(|| panic!("{path} must set RELEASES_REPO"))
    };
    for path in [
        ".github/workflows/release-setup-check.yml",
        ".github/workflows/ci.yml",
    ] {
        assert_eq!(
            releases_repo(path),
            releases_repo(".github/workflows/release.yml"),
            "{path}"
        );
    }
    for path in [
        ".github/workflows/release.yml",
        ".github/workflows/release-setup-check.yml",
        ".github/workflows/ci.yml",
    ] {
        assert!(
            repo_file(path).contains("run: .github/scripts/releases-repo-check.sh"),
            "{path} runs the shared check"
        );
    }
}

/// Replays `verification/vectors/version.json`, computed by the Lean model
/// that `verification/VocaraVerify/Version.lean` proves is a strict order.
#[test]
fn is_newer_matches_the_verified_model() {
    let text = repo_file("rust/verification/vectors/version.json");
    let cases: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
    assert!(cases.len() > 400);
    for case in cases {
        let candidate = case["candidate"].as_str().unwrap();
        let current = case["current"].as_str().unwrap();
        assert_eq!(
            UpdateCheckService::is_newer(candidate, current),
            case["newer"].as_bool().unwrap(),
            "is_newer({candidate:?}, {current:?})"
        );
    }
}
