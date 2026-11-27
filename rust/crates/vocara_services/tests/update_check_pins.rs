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
    // And the one repository a manifest's image may come from.
    assert_eq!(UpdateCheckService::RELEASES_REPO, repo);
}

/// The value of `key:` in release.yml's top-level `env`.
fn workflow_env(workflow: &str, key: &str) -> String {
    workflow
        .lines()
        .find_map(|l| l.trim().strip_prefix(&format!("{key}:")))
        .map(|v| v.trim().to_string())
        .unwrap_or_else(|| panic!("release.yml must set {key}"))
}

/// The manifest release.yml writes names the DMG as the self-update reads
/// it: the address of this release's asset (which the app accepts only from
/// this repository's releases), its size and its SHA-256 — next to the
/// `latest`, `url` and `notes` the versions before it read. A manifest built
/// as the workflow builds one, for a DMG named as the workflow names it,
/// parses with its image.
#[test]
fn the_manifest_release_yml_writes_names_the_dmg_the_app_reads() {
    let workflow = repo_file(".github/workflows/release.yml");
    for written in [
        "{latest: $latest, url: $url, notes: $notes,",
        "dmg: {url: $dmg_url, size: $dmg_size, sha256: $dmg_sha256}}",
        "--arg dmg_url \"https://github.com/$RELEASES_REPO/releases/download/$TAG/$DMG_NAME\"",
        "--argjson dmg_size \"$SIZE\"",
        "DMG=\"dist/Vocara-$VERSION.dmg\"",
    ] {
        assert!(workflow.contains(written), "release.yml: {written}");
    }
    let repo = workflow_env(&workflow, "RELEASES_REPO");
    let (tag, name) = ("v0.4.0", "Vocara-0.4.0.dmg");
    let manifest = serde_json::json!({
        "latest": "0.4.0",
        "url": format!("https://github.com/{repo}/releases/tag/{tag}"),
        "notes": "Vocara 0.4.0\n\nWhat changed.",
        "dmg": {
            "url": format!("https://github.com/{repo}/releases/download/{tag}/{name}"),
            "size": 41_234_567,
            "sha256": "a".repeat(64),
        },
    });
    let info = UpdateCheckService::parse_manifest(&manifest.to_string()).expect("a manifest");
    let dmg = info.dmg.expect("the image, as the workflow names it");
    assert_eq!(dmg.size, 41_234_567);
    assert_eq!(dmg.sha256_hex(), "a".repeat(64));
}

/// The DMG holds the app where the installer looks for it.
#[test]
fn the_dmg_holds_the_app_where_the_installer_looks() {
    use vocara_services::self_update::install::APP_ON_IMAGE;
    let workflow = repo_file(".github/workflows/release.yml");
    assert!(
        workflow.contains(&format!("ditto \"$APP_PATH\" \"$STAGE/{APP_ON_IMAGE}\"")),
        "release.yml stages the app on the image as {APP_ON_IMAGE}"
    );
}

/// The in-app updater demands a notarization ticket only because the
/// release cannot publish a signed DMG without one: the workflow's own
/// step asserts the very predicate the installer evaluates.
#[test]
fn a_signed_dmg_is_a_notarized_dmg_in_the_workflow() {
    use vocara_services::self_update::install::REQUIRE_NOTARIZATION;
    let workflow = repo_file(".github/workflows/release.yml");
    if REQUIRE_NOTARIZATION {
        let step = workflow
            .split("- name: A signed DMG is a notarized DMG")
            .nth(1)
            .expect("release.yml has the step");
        let step = step.split("- name:").next().unwrap_or(step);
        assert!(step.contains("xcrun stapler validate \"$DMG\""), "{step}");
        assert!(
            step.contains(
                "spctl --assess --type open --context context:primary-signature \"$DMG\""
            ),
            "{step}"
        );
        assert!(!step.contains("if:"), "never skipped: {step}");
    }
}

/// The identity every install pins an update to is the shipped app's: the
/// workflow checks the signed build against `UPDATE_IDENTIFIER`, which must
/// be the prod bundle id bundle.sh stamps (the signing identifier codesign
/// takes from it).
#[test]
fn the_pinned_identity_is_the_prod_bundle_id() {
    let workflow = repo_file(".github/workflows/release.yml");
    let bundle = repo_file("rust/packaging/macos/bundle.sh");
    let prod = bundle
        .lines()
        .find_map(|l| l.trim().strip_prefix("prod) BUNDLE_ID="))
        .and_then(|rest| rest.split(';').next())
        .expect("bundle.sh names the prod bundle id");
    assert_eq!(workflow_env(&workflow, "UPDATE_IDENTIFIER"), prod);
    let team = workflow_env(&workflow, "UPDATE_TEAM_ID");
    assert!(
        team.len() == 10 && team.bytes().all(|b| b.is_ascii_alphanumeric()),
        "{team}"
    );
    assert!(workflow
        .contains("[ \"$ID\" != \"$UPDATE_IDENTIFIER\" ] || [ \"$TEAM\" != \"$UPDATE_TEAM_ID\" ]"));
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
