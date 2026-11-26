//! Whether a newer Vocara exists (port of
//! `lib/data/services/update_check_service.dart`).
//!
//! Checks once per app run automatically and on demand from Settings. This is
//! the app's one release-time escape hatch: with no update path, nothing
//! shipped after release would ever reach users.
//!
//! Failure policy: an update check can never make anything worse. Offline,
//! 404, TLS trouble, or a malformed manifest all collapse to
//! [`UpdateStatus::Unknown`] silently (an update already found is kept); only
//! Settings' manual check shows even a gentle "could not check" line.

use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use serde_json::Value;
use vocara_model::diagnostics::DiagnosticsLog;

/// The outcome of the most recent update check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateStatus {
    /// No check has completed yet, or the last check could not reach / parse
    /// the manifest (and none had found an update, which a failure keeps).
    /// Never surfaced as an error on the automatic check.
    Unknown,
    /// The manifest was read and this build is the latest (or newer).
    UpToDate,
    /// The manifest advertises a strictly newer version.
    UpdateAvailable,
}

impl UpdateStatus {
    /// The Dart enum value's name, as the diagnostics log writes it.
    pub fn name(self) -> &'static str {
        match self {
            UpdateStatus::Unknown => "unknown",
            UpdateStatus::UpToDate => "upToDate",
            UpdateStatus::UpdateAvailable => "updateAvailable",
        }
    }
}

/// A parsed "latest release" manifest entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    /// The advertised latest version, normalized (no leading `v`).
    pub version: String,
    /// The https download / release page for that version.
    pub url: String,
    /// Release notes, possibly empty.
    pub notes: String,
}

/// Why a manifest fetch failed (offline, a non-200 status, TLS, timeout).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct FetchError(pub String);

/// Fetches the manifest body for [`UpdateCheckService::MANIFEST_URL`]. The
/// production fetcher does HTTP; tests inject their own.
pub type UpdateFetcher = Box<dyn Fn(&str) -> Result<String, FetchError> + Send + Sync>;

#[derive(Debug)]
struct State {
    status: UpdateStatus,
    available: Option<UpdateInfo>,
    checking: bool,
    auto_check_done: bool,
    notice_dismissed: bool,
    last_checked_at: Option<SystemTime>,
    unreadable: bool,
}

/// The update check (a `ChangeNotifier` in Dart: [`add_listener`] callbacks
/// fire wherever Dart called `notifyListeners`).
///
/// [`add_listener`]: UpdateCheckService::add_listener
pub struct UpdateCheckService {
    fetch: UpdateFetcher,
    current_version: String,
    state: Mutex<State>,
    listeners: Mutex<Vec<Box<dyn Fn() + Send + Sync>>>,
}

impl Default for UpdateCheckService {
    fn default() -> Self {
        Self::new()
    }
}

impl UpdateCheckService {
    /// Where the version manifest lives: the `latest.json` the release
    /// workflow attaches to each release of this public repo (release.yml's
    /// RELEASES_REPO), read without a login. GitHub redirects
    /// `releases/latest/download/` to that file on the newest release that
    /// is not a prerelease. It holds
    ///
    /// ```json
    /// {"latest": "1.1.0", "url": "https://...", "notes": "..."}
    /// ```
    ///
    /// The parser also accepts GitHub's own release shape (`tag_name` /
    /// `html_url` / `body`), so this can point at a public repo's
    /// releases/latest API instead without code changes. A test pins this to
    /// RELEASES_REPO.
    pub const MANIFEST_URL: &'static str =
        "https://github.com/RutaTang/vocara/releases/latest/download/latest.json";

    /// The running app's version. A test pins it to the workspace package
    /// version and to `client/pubspec.yaml` (the part before `+`), so a
    /// release with a stale constant fails the test gate instead of shipping
    /// an app that mis-identifies its own version.
    pub const APP_VERSION: &'static str = "0.1.0";

    /// The running app's build number (pubspec.yaml's part after `+`).
    pub const APP_BUILD_NUMBER: &'static str = "1";

    /// The production service: HTTP fetch, this build's version.
    pub fn new() -> Self {
        Self::with_fetcher(Box::new(fetch_over_http), Self::APP_VERSION)
    }

    /// A service with an injected fetcher and current version (tests).
    pub fn with_fetcher(fetch: UpdateFetcher, current_version: &str) -> Self {
        UpdateCheckService {
            fetch,
            current_version: current_version.to_string(),
            state: Mutex::new(State {
                status: UpdateStatus::Unknown,
                available: None,
                checking: false,
                auto_check_done: false,
                notice_dismissed: false,
                last_checked_at: None,
                unreadable: false,
            }),
            listeners: Mutex::new(Vec::new()),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Registers a callback run on every change (Dart's `addListener`).
    pub fn add_listener(&self, listener: Box<dyn Fn() + Send + Sync>) {
        self.listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(listener);
    }

    fn notify_listeners(&self) {
        for l in self
            .listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
        {
            l();
        }
    }

    /// The outcome of the most recent completed check.
    pub fn status(&self) -> UpdateStatus {
        self.state().status
    }

    /// The newer release, `Some` exactly when [`status`] is
    /// [`UpdateStatus::UpdateAvailable`].
    ///
    /// [`status`]: UpdateCheckService::status
    pub fn available(&self) -> Option<UpdateInfo> {
        self.state().available.clone()
    }

    /// True while a check is in flight.
    pub fn checking(&self) -> bool {
        self.state().checking
    }

    /// When the last check completed (successfully or not; a failure that
    /// kept a found update leaves the time of the check that found it);
    /// `None` before any.
    pub fn last_checked_at(&self) -> Option<SystemTime> {
        self.state().last_checked_at
    }

    /// Whether the last recorded [`UpdateStatus::Unknown`] reached the
    /// manifest but could not read it (an unreadable version or link),
    /// rather than not reaching it: Settings blamed the network for both.
    pub fn manifest_unreadable(&self) -> bool {
        self.state().unreadable
    }

    /// True once the user dismissed the home banner for this app run.
    pub fn notice_dismissed(&self) -> bool {
        self.state().notice_dismissed
    }

    /// Hides the home banner for the rest of this app run (Settings still
    /// shows the available update).
    pub fn dismiss_notice(&self) {
        {
            let mut s = self.state();
            if s.notice_dismissed {
                return;
            }
            s.notice_dismissed = true;
        }
        self.notify_listeners();
    }

    /// The once-per-app-run automatic check (home schedules it shortly after
    /// mounting). Re-entering home later never re-checks; Settings' manual
    /// [`check`] always does. Blocking: run it off the UI thread.
    ///
    /// [`check`]: UpdateCheckService::check
    pub fn auto_check(&self) {
        {
            let mut s = self.state();
            if s.auto_check_done {
                return;
            }
            s.auto_check_done = true;
        }
        self.check();
    }

    /// Fetches and parses the manifest, then updates [`status`] /
    /// [`available`]. Never fails. A failure keeps an update already found
    /// (a manual check made offline, or past GitHub's hourly limit, erased
    /// the one the auto-check had found, banner and all); anything else
    /// degrades to [`UpdateStatus::Unknown`], so Settings says the check
    /// failed rather than "You have the latest version". Blocking: run it off
    /// the UI thread.
    ///
    /// [`status`]: UpdateCheckService::status
    /// [`available`]: UpdateCheckService::available
    pub fn check(&self) {
        {
            let mut s = self.state();
            if s.checking {
                return;
            }
            s.checking = true;
        }
        self.notify_listeners();
        let mut next: Option<UpdateStatus> = None;
        let mut info: Option<UpdateInfo> = None;
        let mut reached = false;
        // Offline / 404 / TLS / timeout: silently unknown. The check must
        // never become an error the user has to deal with.
        if let Ok(body) = (self.fetch)(Self::MANIFEST_URL) {
            reached = true;
            if let Some(parsed) = Self::parse_manifest(&body) {
                if Self::is_newer(&parsed.version, &self.current_version) {
                    next = Some(UpdateStatus::UpdateAvailable);
                    info = Some(parsed);
                } else {
                    next = Some(UpdateStatus::UpToDate);
                }
            }
        }
        {
            let mut s = self.state();
            if next.is_some() || s.status != UpdateStatus::UpdateAvailable {
                s.status = next.unwrap_or(UpdateStatus::Unknown);
                s.available = info.clone();
                s.last_checked_at = Some(SystemTime::now());
                s.unreadable = next.is_none() && reached;
            }
            s.checking = false;
        }
        // Only the version strings ever reach the log — no user content.
        let latest = info
            .as_ref()
            .map(|i| format!(" latest={}", i.version))
            .unwrap_or_default();
        DiagnosticsLog::instance().log(
            "update",
            &format!(
                "check: {}{latest}",
                next.map(UpdateStatus::name).unwrap_or("failed")
            ),
        );
        self.notify_listeners();
    }

    /// Parses a manifest body into an [`UpdateInfo`], or `None` when it isn't
    /// one. Accepts the plain `{latest, url, notes}` shape and GitHub's
    /// releases/latest shape (`tag_name` / `html_url` / `body`). The URL must
    /// be https — a tampered or downgraded manifest must not be able to point
    /// users at a plaintext download page, let alone a non-web URL — and name
    /// a host. The version must be one [`is_newer`] can read: compared as an
    /// unreadable one, it read "You have the latest version" and erased an
    /// update already found.
    ///
    /// `html_url` WINS over `url` when both are present: a real GitHub
    /// release object carries both, and its `url` is the api.github.com
    /// resource endpoint — opening that from "Open Download Page" lands the
    /// user on raw JSON with no installer in sight. A hand-hosted manifest
    /// has no `html_url`, so it still falls through to `url`.
    ///
    /// [`is_newer`]: UpdateCheckService::is_newer
    pub fn parse_manifest(body: &str) -> Option<UpdateInfo> {
        let decoded: Value = serde_json::from_str(body).ok()?;
        let map = decoded.as_object()?;
        // Dart's `a ?? b`: the first key that is present and not null.
        let pick = |a: &str, b: &str| -> Option<&Value> {
            map.get(a)
                .filter(|v| !v.is_null())
                .or_else(|| map.get(b).filter(|v| !v.is_null()))
        };
        let version = pick("latest", "tag_name")?.as_str()?;
        parse_version(version)?;
        let url = pick("html_url", "url")?.as_str()?;
        let trimmed = url.trim();
        if !is_https_with_host(trimmed) {
            return None;
        }
        let notes = pick("notes", "body")
            .and_then(Value::as_str)
            .map(|n| n.trim().to_string())
            .unwrap_or_default();
        let v = version.trim();
        let v = v
            .strip_prefix('v')
            .or_else(|| v.strip_prefix('V'))
            .unwrap_or(v);
        Some(UpdateInfo {
            version: v.to_string(),
            url: trimmed.to_string(),
            notes,
        })
    }

    /// True when [candidate] is a strictly newer dotted-numeric version than
    /// [current]. Tolerates a leading `v` and a `+build` suffix; anything not
    /// purely numeric-dotted (e.g. `1.1.0-rc1`) is conservatively NOT newer,
    /// so a malformed manifest can never raise the banner.
    ///
    /// The CURRENT build alone may carry a `-prerelease` suffix — release.yml
    /// ships `-` tags as prereleases, and an RC install whose own version was
    /// unparseable read "up to date" forever. It compares by its release
    /// core, and the GA of that same core counts as newer (semver: X.Y.Z-pre
    /// < X.Y.Z). A prerelease candidate still never parses, so none is
    /// offered. (Proved a strict order in `verification/VocaraVerify/Version.lean`.)
    pub fn is_newer(candidate: &str, current: &str) -> bool {
        let a = parse_version(candidate);
        // A '-' in +build metadata isn't one.
        let core = match current.find('+') {
            Some(plus) => &current[..plus],
            None => current,
        };
        let dash = core.find('-');
        let b = parse_version(match dash {
            Some(d) => &core[..d],
            None => core,
        });
        let (Some(a), Some(b)) = (a, b) else {
            return false;
        };
        let len = a.len().max(b.len());
        for i in 0..len {
            let x = a.get(i).copied().unwrap_or(0);
            let y = b.get(i).copied().unwrap_or(0);
            if x != y {
                return x > y;
            }
        }
        dash.is_some() // equal cores: the GA beats its own prerelease
    }
}

/// Dart's `Uri.tryParse(url)` then `scheme == 'https' && host.isNotEmpty`
/// (Dart lowercases the scheme, so `HTTPS://` passes too).
fn is_https_with_host(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("https") {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or("");
    let host = if let Some(stripped) = host_port.strip_prefix('[') {
        stripped.split(']').next().unwrap_or("")
    } else {
        host_port.split(':').next().unwrap_or("")
    };
    !host.is_empty()
}

/// A dotted-numeric version with an optional leading `v` and `+build`, or
/// `None`. Digits only: Dart's `int.tryParse` also takes "0x1" and " 2".
fn parse_version(raw: &str) -> Option<Vec<u64>> {
    let mut s = raw.trim();
    if let Some(rest) = s.strip_prefix('v').or_else(|| s.strip_prefix('V')) {
        s = rest;
    }
    if let Some(plus) = s.find('+') {
        s = &s[..plus];
    }
    if s.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    for part in s.split('.') {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        parts.push(part.parse::<u64>().ok()?);
    }
    Some(parts)
}

/// The production fetcher: a blocking HTTPS GET bounded by timeouts, so a
/// wedged connection can't hold resources. Redirects are followed (GitHub's
/// `releases/latest/download/` redirects twice).
fn fetch_over_http(url: &str) -> Result<String, FetchError> {
    let timeout = Duration::from_secs(10);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(timeout))
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into();
    let mut response = agent
        .get(url)
        // GitHub rejects API requests without a User-Agent.
        .header("User-Agent", "vocara-update-check")
        .header("Accept", "application/json")
        .call()
        .map_err(|e| FetchError(e.to_string()))?;
    if response.status() != 200 {
        return Err(FetchError(format!("status {}", response.status())));
    }
    response
        .body_mut()
        .read_to_string()
        .map_err(|e| FetchError(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    fn service(body: &'static str, current: &str) -> UpdateCheckService {
        UpdateCheckService::with_fetcher(Box::new(move |_| Ok(body.to_string())), current)
    }

    fn offline(current: &str) -> UpdateCheckService {
        UpdateCheckService::with_fetcher(Box::new(|_| Err(FetchError("offline".into()))), current)
    }

    // group('parseManifest')

    #[test]
    fn reads_the_plain_shape() {
        let info = UpdateCheckService::parse_manifest(
            r#"{"latest": "1.2.0", "url": "https://example.com/dl", "notes": "Fixes."}"#,
        )
        .unwrap();
        assert_eq!(info.version, "1.2.0");
        assert_eq!(info.url, "https://example.com/dl");
        assert_eq!(info.notes, "Fixes.");
    }

    #[test]
    fn reads_the_github_shape_and_strips_the_v_prefix() {
        let info = UpdateCheckService::parse_manifest(
            r#"{"tag_name": "v1.2.0", "html_url": "https://github.com/RutaTang/vocara/releases/tag/v1.2.0", "body": "Release notes"}"#,
        )
        .unwrap();
        assert_eq!(info.version, "1.2.0");
        assert!(info.url.contains("/releases/tag/v1.2.0"));
        assert_eq!(info.notes, "Release notes");
    }

    #[test]
    fn a_real_github_release_carries_both_urls_and_the_human_page_wins() {
        let info = UpdateCheckService::parse_manifest(
            r#"{"tag_name": "v1.2.0", "url": "https://api.github.com/repos/RutaTang/vocara/releases/123", "html_url": "https://github.com/RutaTang/vocara/releases/tag/v1.2.0", "body": "Release notes"}"#,
        )
        .unwrap();
        assert_eq!(
            info.url,
            "https://github.com/RutaTang/vocara/releases/tag/v1.2.0"
        );
        assert!(!info.url.contains("api.github.com"));
    }

    #[test]
    fn missing_notes_default_to_empty() {
        let info = UpdateCheckService::parse_manifest(
            r#"{"latest": "1.2.0", "url": "https://example.com"}"#,
        )
        .unwrap();
        assert!(info.notes.is_empty());
    }

    #[test]
    fn rejects_malformed_json_non_objects_and_missing_fields() {
        for body in [
            "not json",
            "[1, 2]",
            r#"{"url": "https://x"}"#,
            r#"{"latest": "1.0"}"#,
            r#"{"latest": "", "url": "https://x"}"#,
        ] {
            assert!(UpdateCheckService::parse_manifest(body).is_none(), "{body}");
        }
    }

    #[test]
    fn rejects_non_https_urls_so_a_manifest_cannot_open_anything_else() {
        for url in [
            "file:///Applications",
            "/usr/bin/say",
            "http://example.com/dl",
        ] {
            let body = format!(r#"{{"latest": "9.0.0", "url": "{url}"}}"#);
            assert!(UpdateCheckService::parse_manifest(&body).is_none(), "{url}");
        }
    }

    #[test]
    fn an_unreadable_version_or_a_url_with_no_host_is_no_manifest() {
        for version in ["next", "v", "0x1.0.0", "0. 2.0", "1.0.0-rc1"] {
            let body = format!(r#"{{"latest": "{version}", "url": "https://example.com"}}"#);
            assert!(
                UpdateCheckService::parse_manifest(&body).is_none(),
                "{version}"
            );
        }
        assert!(
            UpdateCheckService::parse_manifest(r#"{"latest": "1.0.0", "url": "https://"}"#)
                .is_none()
        );
        assert!(!UpdateCheckService::is_newer("0x2.0.0", "1.0.0"));
        assert!(!UpdateCheckService::is_newer("1.0.1x9", "1.0.0"));
    }

    // group('isNewer')

    #[test]
    fn strictly_newer_dotted_versions_win() {
        assert!(UpdateCheckService::is_newer("1.0.1", "1.0.0"));
        assert!(UpdateCheckService::is_newer("1.1", "1.0.9"));
        assert!(UpdateCheckService::is_newer("2.0.0", "1.9.9"));
        // Numeric, not lexicographic.
        assert!(UpdateCheckService::is_newer("1.0.10", "1.0.9"));
    }

    #[test]
    fn equal_or_older_is_not_newer() {
        assert!(!UpdateCheckService::is_newer("1.0.0", "1.0.0"));
        assert!(!UpdateCheckService::is_newer("1.0", "1.0.0"));
        assert!(!UpdateCheckService::is_newer("0.9.9", "1.0.0"));
    }

    #[test]
    fn tolerates_v_prefix_and_build_suffix() {
        assert!(UpdateCheckService::is_newer("v1.0.1", "1.0.0"));
        assert!(UpdateCheckService::is_newer("1.0.1+7", "1.0.0"));
        assert!(!UpdateCheckService::is_newer("v1.0.0", "1.0.0"));
    }

    #[test]
    fn unparseable_versions_are_conservatively_not_newer() {
        assert!(!UpdateCheckService::is_newer("abc", "1.0.0"));
        assert!(!UpdateCheckService::is_newer("1.0.0-rc1", "1.0.0"));
        assert!(!UpdateCheckService::is_newer("", "1.0.0"));
        assert!(!UpdateCheckService::is_newer("1.0.1", "garbage"));
    }

    #[test]
    fn a_prerelease_current_build_is_offered_its_ga_and_anything_later() {
        assert!(UpdateCheckService::is_newer("1.0.0", "1.0.0-rc.1"));
        assert!(UpdateCheckService::is_newer("1.0.1", "v1.0.0-rc.1+3"));
        assert!(!UpdateCheckService::is_newer("0.9.9", "1.0.0-rc.1"));
        // A prerelease candidate is still never offered.
        assert!(!UpdateCheckService::is_newer("1.0.0-rc.2", "1.0.0-rc.1"));
        // A '-' inside +build metadata is not a prerelease marker.
        assert!(!UpdateCheckService::is_newer("1.0.0", "1.0.0+build-7"));
    }

    // group('check')

    #[test]
    fn newer_manifest_gives_update_available_with_the_release_info() {
        let s = service(
            r#"{"latest": "9.9.9", "url": "https://example.com/dl", "notes": "Big"}"#,
            "1.0.0",
        );
        s.check();
        assert_eq!(s.status(), UpdateStatus::UpdateAvailable);
        assert_eq!(s.available().unwrap().version, "9.9.9");
        assert!(s.last_checked_at().is_some());
    }

    #[test]
    fn same_version_gives_up_to_date_with_no_available_info() {
        let s = service(
            r#"{"latest": "1.0.0", "url": "https://example.com"}"#,
            "1.0.0",
        );
        s.check();
        assert_eq!(s.status(), UpdateStatus::UpToDate);
        assert!(s.available().is_none());
    }

    #[test]
    fn a_failing_fetcher_degrades_to_unknown() {
        let s = offline("1.0.0");
        s.check();
        assert_eq!(s.status(), UpdateStatus::Unknown);
        assert!(s.available().is_none());
        assert!(s.last_checked_at().is_some());
    }

    #[test]
    fn an_unreadable_manifest_is_told_apart_from_an_unreachable_one() {
        // Settings blamed the network for a manifest it had read.
        let unreadable = service(
            r#"{"latest": "next", "url": "https://example.com"}"#,
            "1.0.0",
        );
        unreadable.check();
        assert_eq!(unreadable.status(), UpdateStatus::Unknown);
        assert!(unreadable.manifest_unreadable());
        let off = offline("1.0.0");
        off.check();
        assert_eq!(off.status(), UpdateStatus::Unknown);
        assert!(!off.manifest_unreadable());
    }

    #[test]
    fn a_malformed_body_degrades_to_unknown() {
        let s = service("<html>rate limited</html>", "1.0.0");
        s.check();
        assert_eq!(s.status(), UpdateStatus::Unknown);
    }

    fn flaky(fail: Arc<AtomicBool>, body: &'static str) -> UpdateCheckService {
        UpdateCheckService::with_fetcher(
            Box::new(move |_| {
                if fail.load(Ordering::SeqCst) {
                    Err(FetchError("offline".into()))
                } else {
                    Ok(body.to_string())
                }
            }),
            "1.0.0",
        )
    }

    #[test]
    fn a_later_successful_check_replaces_an_unknown_result() {
        let fail = Arc::new(AtomicBool::new(true));
        let s = flaky(
            fail.clone(),
            r#"{"latest": "2.0.0", "url": "https://example.com"}"#,
        );
        s.check();
        assert_eq!(s.status(), UpdateStatus::Unknown);
        fail.store(false, Ordering::SeqCst);
        s.check();
        assert_eq!(s.status(), UpdateStatus::UpdateAvailable);
    }

    #[test]
    fn a_failed_check_keeps_the_last_good_result() {
        // A manual check made offline (or past GitHub's hourly limit) erased
        // an update the auto-check had found.
        let fail = Arc::new(AtomicBool::new(false));
        let s = flaky(
            fail.clone(),
            r#"{"latest": "2.0.0", "url": "https://example.com"}"#,
        );
        s.check();
        assert_eq!(s.status(), UpdateStatus::UpdateAvailable);
        let checked_at = s.last_checked_at();
        fail.store(true, Ordering::SeqCst);
        s.check();
        assert_eq!(s.status(), UpdateStatus::UpdateAvailable);
        assert_eq!(s.available().unwrap().version, "2.0.0");
        assert_eq!(s.last_checked_at(), checked_at);
    }

    #[test]
    fn a_failed_check_after_up_to_date_says_it_failed() {
        let fail = Arc::new(AtomicBool::new(false));
        let s = flaky(
            fail.clone(),
            r#"{"latest": "1.0.0", "url": "https://example.com"}"#,
        );
        s.check();
        assert_eq!(s.status(), UpdateStatus::UpToDate);
        fail.store(true, Ordering::SeqCst);
        s.check();
        assert_eq!(s.status(), UpdateStatus::Unknown);
    }

    #[test]
    fn notifies_listeners_when_the_check_starts_and_completes() {
        let notifications = Arc::new(AtomicUsize::new(0));
        let s = service(
            r#"{"latest": "1.0.0", "url": "https://example.com"}"#,
            "1.0.0",
        );
        let n = notifications.clone();
        s.add_listener(Box::new(move || {
            n.fetch_add(1, Ordering::SeqCst);
        }));
        s.check();
        assert_eq!(notifications.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn auto_check_fetches_exactly_once_per_app_run_and_a_manual_check_still_works() {
        let fetches = Arc::new(AtomicUsize::new(0));
        let f = fetches.clone();
        let s = UpdateCheckService::with_fetcher(
            Box::new(move |_| {
                f.fetch_add(1, Ordering::SeqCst);
                Ok(r#"{"latest": "1.0.0", "url": "https://example.com"}"#.to_string())
            }),
            "1.0.0",
        );
        s.auto_check();
        s.auto_check();
        s.auto_check();
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
        s.check();
        assert_eq!(fetches.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn dismiss_notice_sticks_for_the_run_and_notifies_once() {
        let notifications = Arc::new(AtomicUsize::new(0));
        let s = service(
            r#"{"latest": "9.0.0", "url": "https://example.com"}"#,
            "1.0.0",
        );
        s.check();
        assert!(!s.notice_dismissed());
        let n = notifications.clone();
        s.add_listener(Box::new(move || {
            n.fetch_add(1, Ordering::SeqCst);
        }));
        s.dismiss_notice();
        s.dismiss_notice();
        assert!(s.notice_dismissed());
        assert_eq!(notifications.load(Ordering::SeqCst), 1);
    }
}
