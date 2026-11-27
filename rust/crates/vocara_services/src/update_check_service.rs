//! Whether a newer Vocara exists (port of
//! `lib/data/services/update_check_service.dart`), and whether Vocara may
//! look on its own.
//!
//! Checks automatically once per app run, but only once the user has agreed
//! to ([`UpdateCheckService::auto_check`]): the check contacts GitHub, so an
//! unanswered question is not consent, and until it is answered no automatic
//! check runs (Home asks it once, Settings changes the answer). And on demand
//! from Settings. This is the app's one release-time escape hatch: with no
//! update path, nothing shipped after release would ever reach users.
//!
//! The manifest also names the release's disk image ([`DmgAsset`]: its
//! address, size and SHA-256), which the self-update (`crate::self_update`)
//! downloads, checks and installs. A manifest without one (every release
//! before the self-update, or one whose image entry is unusable) is still a
//! release on offer: the app then sends the user to its page, as it always
//! did.
//!
//! Failure policy: an update check can never make anything worse. Offline,
//! 404, TLS trouble, or a malformed manifest all collapse to
//! [`UpdateStatus::Unknown`] silently (an update already found is kept); only
//! Settings' manual check shows even a gentle "could not check" line.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde_json::{Map, Value};
use vocara_model::diagnostics::DiagnosticsLog;

use crate::app_settings_service::SettingsStorage;

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
    /// The release's disk image, for the self-update; `None` when the
    /// manifest names none Vocara can use (the release page is then the only
    /// way).
    pub dmg: Option<DmgAsset>,
}

/// The disk image a manifest names: where it is, how big it is, and its
/// SHA-256.
///
/// What these buy is integrity, not trust: a download that is not exactly
/// `size` bytes with this digest is thrown away, but the manifest is only a
/// file on a web server, so an image that matches it is still untrusted bytes
/// until the installer has checked the app inside it against the running
/// app's own signing identity (`self_update::install`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmgAsset {
    /// The image's address: an https release asset of this repository on
    /// github.com ([`is_release_asset_url`]).
    pub url: String,
    /// Its exact size in bytes, at most `self_update::MAX_UPDATE_BYTES`.
    pub size: u64,
    /// Its SHA-256.
    pub sha256: [u8; 32],
}

impl DmgAsset {
    /// The digest as the manifest and `SHA256SUMS` write it: 64 lowercase
    /// hex digits.
    pub fn sha256_hex(&self) -> String {
        hex::encode(self.sha256)
    }
}

/// What [`UpdateCheckService::auto_check`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoCheck {
    /// It already ran this app run.
    AlreadyRan,
    /// The user has not said whether Vocara may check on its own: nothing
    /// was fetched, and the question is due.
    Unanswered,
    /// The user said no: nothing was fetched.
    Declined,
    /// The manifest was fetched (see [`UpdateCheckService::status`]).
    Checked,
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
    /// Where the answer to "check for updates automatically?" is kept
    /// (`settings/updates.json`); `None` keeps it in memory only (a test's
    /// service).
    preference_storage: Option<Arc<dyn SettingsStorage>>,
    /// That answer as last read or chosen: the outer `None` until it has
    /// been read, the inner one while the user has not answered.
    preference: Mutex<Option<Option<bool>>>,
    /// Whether this run has put the question already (once per run, however
    /// often Home mounts).
    consent_asked: AtomicBool,
}

/// The document the answer to "check automatically?" is kept in, under the
/// settings collection, apart from the audio settings' own.
const PREFERENCE_COLLECTION: &str = "settings";
const PREFERENCE_DOC: &str = "updates";
const K_AUTO_CHECK: &str = "autoCheck";

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
    /// and, from the self-update on, the release's disk image too:
    ///
    /// ```json
    /// {"latest": "1.1.0", "url": "https://...", "notes": "...",
    ///  "dmg": {"url": "https://github.com/RutaTang/vocara/releases/download/v1.1.0/Vocara-1.1.0.dmg",
    ///          "size": 41234567, "sha256": "<64 hex digits>"}}
    /// ```
    ///
    /// (`latest`, `url` and `notes` stay as they were, so the versions
    /// before the self-update read the new manifest as they read the old
    /// one, and offer its release page.)
    ///
    /// The parser also accepts GitHub's own release shape (`tag_name` /
    /// `html_url` / `body`), so this can point at a public repo's
    /// releases/latest API instead without code changes. A test pins this to
    /// RELEASES_REPO.
    pub const MANIFEST_URL: &'static str =
        "https://github.com/RutaTang/vocara/releases/latest/download/latest.json";

    /// The repository the releases are published on (release.yml's
    /// `RELEASES_REPO`; a test pins the two together, and [`MANIFEST_URL`]
    /// to it): the only repository a manifest's disk image may come from.
    ///
    /// [`MANIFEST_URL`]: UpdateCheckService::MANIFEST_URL
    pub const RELEASES_REPO: &'static str = "RutaTang/vocara";

    /// The version this binary was built as. A test pins it to the workspace
    /// package version (which the release tag must match), so a release with
    /// a stale constant fails the test gate instead of shipping an app that
    /// mis-identifies its own version. What the app compares releases with
    /// is the RUNNING version, the bundle's own
    /// (`self_update::current_version`), which falls back to this outside a
    /// bundle; release.yml checks that the bundle it builds says the same.
    pub const APP_VERSION: &'static str = "0.4.0";

    /// The running app's build number: the bundle's `CFBundleVersion`
    /// (`packaging/macos/bundle.sh` reads it here), one more each release.
    pub const APP_BUILD_NUMBER: &'static str = "4";

    /// The production service: HTTP fetch, compared with the running
    /// version (`self_update::current_version`: the bundle's), the answer to
    /// "check automatically?" kept in `preferences` (the app's storage).
    pub fn new(preferences: Arc<dyn SettingsStorage>) -> Self {
        Self::with_fetcher(
            Box::new(fetch_over_http),
            crate::self_update::current_version(),
        )
        .with_preferences(preferences)
    }

    /// The same service, the answer to "check automatically?" kept in
    /// `storage` (`settings/updates.json`) and read from it the first time it
    /// is asked for.
    pub fn with_preferences(mut self, storage: Arc<dyn SettingsStorage>) -> Self {
        self.preference_storage = Some(storage);
        self.preference = Mutex::new(None);
        self
    }

    /// A service with an injected fetcher and current version (tests). The
    /// answer to "check automatically?" is kept in memory, unanswered until
    /// [`set_auto_check`](UpdateCheckService::set_auto_check).
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
            preference_storage: None,
            preference: Mutex::new(Some(None)),
            consent_asked: AtomicBool::new(false),
        }
    }

    /// The version releases are compared with.
    pub fn current_version(&self) -> &str {
        &self.current_version
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
    /// mounting), made only when the user agreed to it
    /// ([`auto_check_preference`]): unanswered, it fetches nothing and says
    /// the question is due; declined, it fetches nothing. Re-entering home
    /// later never re-checks; Settings' manual [`check`] always does.
    /// Blocking (the answer's first read, the fetch): run it off the UI
    /// thread.
    ///
    /// [`check`]: UpdateCheckService::check
    /// [`auto_check_preference`]: UpdateCheckService::auto_check_preference
    pub fn auto_check(&self) -> AutoCheck {
        {
            let mut s = self.state();
            if s.auto_check_done {
                return AutoCheck::AlreadyRan;
            }
            s.auto_check_done = true;
        }
        match self.auto_check_preference() {
            None => AutoCheck::Unanswered,
            Some(false) => AutoCheck::Declined,
            Some(true) => {
                self.check();
                AutoCheck::Checked
            }
        }
    }

    /// The answer to "check for updates automatically?": `None` while the
    /// user has not given one. Read from storage the first time (blocking:
    /// off the UI thread), from memory after. A document that is there but
    /// cannot be read is no answer, and is read again next time: taken as
    /// "no" it would silence the check for good, taken as "yes" it would
    /// contact GitHub unasked.
    pub fn auto_check_preference(&self) -> Option<bool> {
        if let Some(known) = *self.preference_lock() {
            return known;
        }
        let Some(storage) = &self.preference_storage else {
            return None;
        };
        let read = match storage.read(PREFERENCE_COLLECTION, PREFERENCE_DOC) {
            Ok(document) => document
                .as_ref()
                .and_then(|d| d.get(K_AUTO_CHECK))
                .and_then(Value::as_bool),
            Err(e) => {
                DiagnosticsLog::instance()
                    .log("update", &format!("preference unreadable: {}", e.kind()));
                return None;
            }
        };
        let mut cached = self.preference_lock();
        // A choice made while this read ran is newer than what it read.
        *cached.get_or_insert(read)
    }

    /// The answer as far as it is known now, without reading storage (for a
    /// view): `None` while it is unanswered or not yet read.
    pub fn auto_check_preference_now(&self) -> Option<bool> {
        self.preference_lock().flatten()
    }

    /// Whether automatic checks are on, as far as is known now.
    pub fn auto_check_enabled(&self) -> bool {
        self.auto_check_preference_now() == Some(true)
    }

    /// Records the answer at once (the switch and the question read it
    /// back straight away) and notifies; [`save_auto_check`] then writes it.
    ///
    /// [`save_auto_check`]: UpdateCheckService::save_auto_check
    pub fn set_auto_check(&self, enabled: bool) {
        *self.preference_lock() = Some(Some(enabled));
        self.notify_listeners();
    }

    /// Writes the answer [`set_auto_check`] recorded, so the next launch
    /// knows it. Blocking: run it off the UI thread. Nothing to write while
    /// there is no answer, or for a test's service (kept in memory).
    ///
    /// [`set_auto_check`]: UpdateCheckService::set_auto_check
    pub fn save_auto_check(&self) -> io::Result<()> {
        let Some(Some(enabled)) = *self.preference_lock() else {
            return Ok(());
        };
        let Some(storage) = &self.preference_storage else {
            return Ok(());
        };
        let mut document = Map::new();
        document.insert(K_AUTO_CHECK.to_string(), Value::Bool(enabled));
        storage.write(PREFERENCE_COLLECTION, PREFERENCE_DOC, &document)
    }

    /// True the first time this run asks the question "check for updates
    /// automatically?", false after: Home asks it once per run, however
    /// often it is mounted, and only while it is unanswered.
    pub fn claim_consent_question(&self) -> bool {
        !self.consent_asked.swap(true, Ordering::SeqCst)
    }

    fn preference_lock(&self) -> std::sync::MutexGuard<'_, Option<Option<bool>>> {
        self.preference.lock().unwrap_or_else(|e| e.into_inner())
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
    /// The disk image (`dmg`) is optional and judged on its own
    /// ([`parse_dmg`]): an entry Vocara cannot use leaves the release on offer
    /// without it (its page is then the way to it), rather than taking the
    /// whole offer away.
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
        Some(UpdateInfo {
            version: normalize_version(version),
            url: trimmed.to_string(),
            notes,
            dmg: map.get("dmg").and_then(parse_dmg),
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

    /// True when `a` and `b` are both readable versions and neither is newer
    /// than the other under [`is_newer`]'s order (`1.2` and `v1.2.0` are the
    /// same version). What the installer asks of an update image: that it
    /// holds exactly the release the user was offered.
    ///
    /// [`is_newer`]: UpdateCheckService::is_newer
    pub fn same_version(a: &str, b: &str) -> bool {
        parse_version(a).is_some()
            && parse_version(b).is_some()
            && !Self::is_newer(a, b)
            && !Self::is_newer(b, a)
    }
}

/// `version` as the app shows it: trimmed, its leading `v` dropped.
pub fn normalize_version(version: &str) -> String {
    let v = version.trim();
    v.strip_prefix('v')
        .or_else(|| v.strip_prefix('V'))
        .unwrap_or(v)
        .to_string()
}

/// A manifest's `dmg` entry, or `None` when Vocara cannot use it: not an
/// object; a `url` that is not one of this repository's release assets
/// ([`is_release_asset_url`]); a `size` that is not a whole number of bytes
/// from 1 to `self_update::MAX_UPDATE_BYTES`; a `sha256` that is not 64 hex
/// digits. Each is refused rather than repaired: the image is what the
/// self-update downloads and installs, and a manifest that gets any of it
/// wrong is not one to act on.
pub fn parse_dmg(value: &Value) -> Option<DmgAsset> {
    let entry = value.as_object()?;
    let url = entry.get("url")?.as_str()?.trim();
    if !is_release_asset_url(url) {
        return None;
    }
    let size = entry.get("size")?.as_u64()?;
    if size == 0 || size > crate::self_update::MAX_UPDATE_BYTES {
        return None;
    }
    let digest = entry.get("sha256")?.as_str()?.trim();
    if digest.len() != 64 {
        return None;
    }
    let mut sha256 = [0u8; 32];
    hex::decode_to_slice(digest, &mut sha256).ok()?;
    Some(DmgAsset {
        url: url.to_string(),
        size,
        sha256,
    })
}

/// Whether `url` is a release asset of [`UpdateCheckService::RELEASES_REPO`]
/// on github.com, as release.yml writes one:
/// `https://github.com/RutaTang/vocara/releases/download/<tag>/<name>.dmg`.
///
/// Judged strictly on the text, because it is the text that gets fetched:
/// no user info, no port, no query or fragment, no percent escapes (a
/// `%2e%2e` segment passes a prefix test and resolves elsewhere), exactly
/// two plain segments after `download/` (letters, digits, `.`, `_`, `-`,
/// `+`; never `.` or `..`). It is not what makes an update safe — the
/// installer checks whatever arrives — but it keeps Vocara from being told
/// to fetch half a gigabyte from an arbitrary host in the first place.
/// GitHub then redirects the download to its own storage host, over https
/// only (`self_update::download`).
pub fn is_release_asset_url(url: &str) -> bool {
    let prefix = format!(
        "https://github.com/{}/releases/download/",
        UpdateCheckService::RELEASES_REPO
    );
    let Some(rest) = url.strip_prefix(&prefix) else {
        return false;
    };
    let mut segments = rest.split('/');
    let (Some(tag), Some(name), None) = (segments.next(), segments.next(), segments.next()) else {
        return false;
    };
    let plain = |segment: &str| {
        !segment.is_empty()
            && segment != "."
            && segment != ".."
            && segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+'))
    };
    plain(tag) && plain(name) && name.len() > ".dmg".len() && name.ends_with(".dmg")
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
pub(crate) fn parse_version(raw: &str) -> Option<Vec<u64>> {
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

    fn counting(fetches: &Arc<AtomicUsize>) -> UpdateCheckService {
        let f = fetches.clone();
        UpdateCheckService::with_fetcher(
            Box::new(move |_| {
                f.fetch_add(1, Ordering::SeqCst);
                Ok(r#"{"latest": "1.0.0", "url": "https://example.com"}"#.to_string())
            }),
            "1.0.0",
        )
    }

    #[test]
    fn auto_check_fetches_exactly_once_per_app_run_and_a_manual_check_still_works() {
        let fetches = Arc::new(AtomicUsize::new(0));
        let s = counting(&fetches);
        s.set_auto_check(true);
        assert_eq!(s.auto_check(), AutoCheck::Checked);
        assert_eq!(s.auto_check(), AutoCheck::AlreadyRan);
        assert_eq!(s.auto_check(), AutoCheck::AlreadyRan);
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
        s.check();
        assert_eq!(fetches.load(Ordering::SeqCst), 2);
    }

    /// Unanswered is not consent: the automatic check contacts nobody until
    /// the user said yes, and says the question is due; a no is a no.
    #[test]
    fn the_automatic_check_waits_for_consent() {
        let fetches = Arc::new(AtomicUsize::new(0));
        let unanswered = counting(&fetches);
        assert_eq!(unanswered.auto_check_preference(), None);
        assert!(!unanswered.auto_check_enabled());
        assert_eq!(unanswered.auto_check(), AutoCheck::Unanswered);
        let declined = counting(&fetches);
        declined.set_auto_check(false);
        assert_eq!(declined.auto_check(), AutoCheck::Declined);
        assert_eq!(
            fetches.load(Ordering::SeqCst),
            0,
            "no network without a yes"
        );
        // The question is put once per run.
        assert!(unanswered.claim_consent_question());
        assert!(!unanswered.claim_consent_question());
        // A manual check is the user's own asking.
        unanswered.check();
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
    }

    /// The settings document a test keeps in memory, failing its reads or
    /// writes on demand.
    #[derive(Default)]
    struct Documents {
        docs: Mutex<std::collections::HashMap<String, Map<String, Value>>>,
        unreadable: AtomicBool,
        reads: AtomicUsize,
    }

    impl SettingsStorage for Documents {
        fn read(&self, collection: &str, id: &str) -> io::Result<Option<Map<String, Value>>> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            if self.unreadable.load(Ordering::SeqCst) {
                return Err(io::Error::other("locked"));
            }
            Ok(self
                .docs
                .lock()
                .unwrap()
                .get(&format!("{collection}/{id}"))
                .cloned())
        }

        fn write(&self, collection: &str, id: &str, json: &Map<String, Value>) -> io::Result<()> {
            self.docs
                .lock()
                .unwrap()
                .insert(format!("{collection}/{id}"), json.clone());
            Ok(())
        }
    }

    fn kept_in(documents: &Arc<Documents>) -> UpdateCheckService {
        UpdateCheckService::with_fetcher(Box::new(|_| Err(FetchError("offline".into()))), "1.0.0")
            .with_preferences(documents.clone())
    }

    /// The answer is kept in `settings/updates.json`, read once, and survives
    /// a restart; an unreadable document is no answer, and is read again.
    #[test]
    fn the_answer_is_kept_across_runs() {
        let documents = Arc::new(Documents::default());
        let first = kept_in(&documents);
        assert_eq!(first.auto_check_preference_now(), None, "not read yet");
        assert_eq!(first.auto_check_preference(), None);
        assert_eq!(first.auto_check_preference(), None);
        assert_eq!(documents.reads.load(Ordering::SeqCst), 1, "read once");
        first.set_auto_check(true);
        assert!(first.auto_check_enabled(), "at once, before it is written");
        first.save_auto_check().unwrap();
        let written = documents
            .docs
            .lock()
            .unwrap()
            .get("settings/updates")
            .cloned();
        assert_eq!(written.unwrap().get("autoCheck"), Some(&Value::Bool(true)));

        let next_run = kept_in(&documents);
        assert_eq!(next_run.auto_check_preference(), Some(true));
        next_run.set_auto_check(false);
        next_run.save_auto_check().unwrap();
        assert_eq!(kept_in(&documents).auto_check_preference(), Some(false));

        documents.unreadable.store(true, Ordering::SeqCst);
        let locked = kept_in(&documents);
        assert_eq!(locked.auto_check_preference(), None);
        documents.unreadable.store(false, Ordering::SeqCst);
        assert_eq!(locked.auto_check_preference(), Some(false), "read again");
    }

    /// Choosing notifies, so the Settings switch and the Home question read
    /// it back at once.
    #[test]
    fn choosing_notifies() {
        let s = counting(&Arc::new(AtomicUsize::new(0)));
        let heard = Arc::new(AtomicUsize::new(0));
        let h = heard.clone();
        s.add_listener(Box::new(move || {
            h.fetch_add(1, Ordering::SeqCst);
        }));
        s.set_auto_check(true);
        assert_eq!(heard.load(Ordering::SeqCst), 1);
        assert_eq!(
            s.save_auto_check().map_err(|e| e.to_string()),
            Ok(()),
            "memory only"
        );
    }

    // group('the release image')

    const SHA: &str = "4f1e9b5a3c2d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b9c0d1e2f";

    fn with_dmg(dmg: &str) -> String {
        format!(
            r#"{{"latest": "0.4.0", "url": "https://github.com/RutaTang/vocara/releases/tag/v0.4.0", "notes": "Fixes.", "dmg": {dmg}}}"#
        )
    }

    /// The new manifest names the image: its address, size and digest.
    #[test]
    fn the_new_manifest_names_its_image() {
        let body = with_dmg(&format!(
            r#"{{"url": "https://github.com/RutaTang/vocara/releases/download/v0.4.0/Vocara-0.4.0.dmg", "size": 41234567, "sha256": "{SHA}"}}"#
        ));
        let info = UpdateCheckService::parse_manifest(&body).unwrap();
        assert_eq!(info.version, "0.4.0");
        assert_eq!(
            info.url,
            "https://github.com/RutaTang/vocara/releases/tag/v0.4.0"
        );
        let dmg = info.dmg.expect("the image");
        assert_eq!(
            dmg.url,
            "https://github.com/RutaTang/vocara/releases/download/v0.4.0/Vocara-0.4.0.dmg"
        );
        assert_eq!(dmg.size, 41234567);
        assert_eq!(dmg.sha256_hex(), SHA);
        // An upper-case digest is the same digest.
        let upper = body.replace(SHA, &SHA.to_uppercase());
        assert_eq!(
            UpdateCheckService::parse_manifest(&upper)
                .unwrap()
                .dmg
                .unwrap()
                .sha256_hex(),
            SHA
        );
    }

    /// The old shape (every release before the self-update) is still a
    /// release on offer, with no image: its page is the way.
    #[test]
    fn the_old_manifest_is_still_an_offer_without_an_image() {
        let info = UpdateCheckService::parse_manifest(
            r#"{"latest": "0.3.0", "url": "https://github.com/RutaTang/vocara/releases/tag/v0.3.0", "notes": "n"}"#,
        )
        .unwrap();
        assert_eq!(info.dmg, None);
        assert_eq!(info.version, "0.3.0");
    }

    /// An image entry Vocara cannot use leaves the offer, without the image:
    /// another host or repository, plain http, escapes and dot segments, a
    /// port or user info, a query, no `.dmg`; a size of nothing or past the
    /// cap, or not a whole number; a digest that is not 64 hex digits.
    #[test]
    fn an_unusable_image_entry_leaves_the_offer_without_it() {
        let good_url =
            "https://github.com/RutaTang/vocara/releases/download/v0.4.0/Vocara-0.4.0.dmg";
        let entry = |url: &str, size: &str, sha: &str| {
            with_dmg(&format!(
                r#"{{"url": "{url}", "size": {size}, "sha256": "{sha}"}}"#
            ))
        };
        let mut bodies = Vec::new();
        for url in [
            "http://github.com/RutaTang/vocara/releases/download/v0.4.0/Vocara-0.4.0.dmg",
            "https://evil.example/RutaTang/vocara/releases/download/v0.4.0/Vocara-0.4.0.dmg",
            "https://github.com.evil.example/RutaTang/vocara/releases/download/v0.4.0/V.dmg",
            "https://github.com@evil.example/RutaTang/vocara/releases/download/v0.4.0/V.dmg",
            "https://github.com:8443/RutaTang/vocara/releases/download/v0.4.0/V.dmg",
            "https://github.com/someone/else/releases/download/v0.4.0/Vocara-0.4.0.dmg",
            "https://github.com/RutaTang/vocara/releases/download/%2e%2e/%2e%2e/x/y.dmg",
            "https://github.com/RutaTang/vocara/releases/download/../../../x/releases/download/v1/V.dmg",
            "https://github.com/RutaTang/vocara/releases/download/v0.4.0/../V.dmg",
            "https://github.com/RutaTang/vocara/releases/download/v0.4.0/Vocara-0.4.0.dmg?x=1",
            "https://github.com/RutaTang/vocara/releases/download/v0.4.0/Vocara-0.4.0.dmg#x",
            "https://github.com/RutaTang/vocara/releases/download/v0.4.0/Vocara-0.4.0.zip",
            "https://github.com/RutaTang/vocara/releases/download/v0.4.0/.dmg",
            "https://github.com/RutaTang/vocara/releases/download/Vocara-0.4.0.dmg",
            "https://github.com/RutaTang/vocara/releases/download/v0.4.0/a/Vocara-0.4.0.dmg",
            "https://github.com/RutaTang/vocara/releases/download/v0.4.0/Vocara 0.4.0.dmg",
            "file:///Applications/vocara.app",
        ] {
            assert!(!is_release_asset_url(url), "{url}");
            bodies.push(entry(url, "100", SHA));
        }
        for size in ["0", "-1", "1.5", "\"100\"", "536870913", "null"] {
            bodies.push(entry(good_url, size, SHA));
        }
        for sha in [&SHA[1..], "zz", &format!("{}g", &SHA[1..]), ""] {
            bodies.push(entry(good_url, "100", sha));
        }
        bodies.push(with_dmg(r#""https://github.com/x.dmg""#));
        bodies.push(with_dmg("null"));
        for body in bodies {
            let info = UpdateCheckService::parse_manifest(&body).expect(&body);
            assert_eq!(info.dmg, None, "{body}");
            assert_eq!(info.version, "0.4.0");
        }
        assert!(is_release_asset_url(good_url));
        assert!(is_release_asset_url(
            "https://github.com/RutaTang/vocara/releases/download/v1.0.0-rc.1/Vocara-1.0.0-rc.1.dmg"
        ));
    }

    #[test]
    fn the_same_version_is_the_same_however_spelled() {
        assert!(UpdateCheckService::same_version("1.2", "v1.2.0"));
        assert!(UpdateCheckService::same_version("0.4.0", "0.4.0"));
        assert!(!UpdateCheckService::same_version("0.4.1", "0.4.0"));
        assert!(!UpdateCheckService::same_version("next", "next"));
        assert!(!UpdateCheckService::same_version("1.0.0-rc.1", "1.0.0"));
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
