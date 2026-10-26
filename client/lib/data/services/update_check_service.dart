import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';

import 'diagnostics_log.dart';

/// The outcome of the most recent update check.
enum UpdateStatus {
  /// No check has completed yet, or the last check could not reach / parse
  /// the manifest. Never surfaced as an error on the automatic check.
  unknown,

  /// The manifest was read and this build is the latest (or newer).
  upToDate,

  /// The manifest advertises a strictly newer version.
  updateAvailable,
}

/// A parsed "latest release" manifest entry.
@immutable
class UpdateInfo {
  const UpdateInfo({
    required this.version,
    required this.url,
    required this.notes,
  });

  /// The advertised latest version, normalized (no leading `v`).
  final String version;

  /// The https download / release page for that version.
  final String url;

  /// Release notes, possibly empty.
  final String notes;
}

/// Test seam: fetches the manifest body for [UpdateCheckService.manifestUrl].
typedef UpdateFetcher = Future<String> Function(Uri url);

/// Checks whether a newer Vocara exists, once per app run automatically and
/// on demand from Settings. This is the app's one release-time escape hatch:
/// with no update path, nothing shipped after release would ever reach users.
///
/// Failure policy: an update check can never make anything worse. Offline,
/// 404, TLS trouble, or a malformed manifest all collapse to
/// [UpdateStatus.unknown] silently; only Settings' manual check shows even a
/// gentle "could not check" line.
class UpdateCheckService extends ChangeNotifier {
  UpdateCheckService({UpdateFetcher? fetch, String currentVersion = appVersion})
    : _fetch = fetch ?? _fetchOverHttp,
      _currentVersion = currentVersion;

  /// Where the version manifest lives.
  ///
  /// OWNER: change this when the release process settles. The default is
  /// derived from `git remote get-url origin`
  /// (https://github.com/RutaTang/vocara): GitHub's "latest release" API,
  /// which serves unauthenticated JSON as soon as the repo (or a dedicated
  /// public releases repo) is public and has at least one release. The parser
  /// accepts EITHER GitHub's release shape (`tag_name` / `html_url` / `body`)
  /// OR a plain hand-hosted manifest:
  ///
  ///     {"latest": "1.1.0", "url": "https://...", "notes": "..."}
  ///
  /// so pointing this at a static JSON file (e.g. GitHub raw or any web host)
  /// also works without code changes.
  static const String manifestUrl =
      'https://api.github.com/repos/RutaTang/vocara/releases/latest';

  /// The running app's version. Keep in sync with pubspec.yaml `version:`
  /// (the part before `+`). Vocara does not depend on package_info_plus
  /// directly, so this const is the single source the check compares against.
  /// A unit test (`update_check_version_sync_test.dart`) pins it to
  /// pubspec.yaml, so a release with a stale constant fails CI's test gate
  /// instead of shipping an app that mis-identifies its own version.
  static const String appVersion = '0.1.0';

  /// The running app's build number. Keep in sync with pubspec.yaml
  /// `version:` (the part after `+`); pinned by the same unit test.
  static const String appBuildNumber = '1';

  final UpdateFetcher _fetch;
  final String _currentVersion;

  UpdateStatus _status = UpdateStatus.unknown;
  UpdateInfo? _available;
  bool _checking = false;
  bool _autoCheckDone = false;
  bool _noticeDismissed = false;
  DateTime? _lastCheckedAt;

  /// The outcome of the most recent completed check.
  UpdateStatus get status => _status;

  /// The newer release, non-null exactly when [status] is
  /// [UpdateStatus.updateAvailable].
  UpdateInfo? get available => _available;

  /// True while a check is in flight.
  bool get checking => _checking;

  /// When the last check completed (successfully or not); null before any.
  DateTime? get lastCheckedAt => _lastCheckedAt;

  /// True once the user dismissed the home banner for this app run.
  bool get noticeDismissed => _noticeDismissed;

  /// Hides the home banner for the rest of this app run (Settings still
  /// shows the available update).
  void dismissNotice() {
    if (_noticeDismissed) return;
    _noticeDismissed = true;
    notifyListeners();
  }

  /// The once-per-app-run automatic check (home schedules it shortly after
  /// mounting). Re-entering home later never re-checks; Settings' manual
  /// [check] always does.
  Future<void> autoCheck() {
    if (_autoCheckDone) return Future<void>.value();
    _autoCheckDone = true;
    return check();
  }

  /// Fetches and parses the manifest, then updates [status] / [available].
  /// Never throws. A failure keeps the last good result (or leaves
  /// [UpdateStatus.unknown] when there is none): a manual check made offline,
  /// or past GitHub's hourly limit, erased an update the auto-check had found,
  /// banner and all.
  Future<void> check() async {
    if (_checking) return;
    _checking = true;
    notifyListeners();
    UpdateStatus? next;
    UpdateInfo? info;
    try {
      final body = await _fetch(Uri.parse(manifestUrl));
      final parsed = parseManifest(body);
      if (parsed != null) {
        if (isNewer(parsed.version, _currentVersion)) {
          next = UpdateStatus.updateAvailable;
          info = parsed;
        } else {
          next = UpdateStatus.upToDate;
        }
      }
    } catch (_) {
      // Offline / 404 / TLS / timeout: silently unknown. The check must never
      // become an error the user has to deal with.
    }
    if (next != null) {
      _status = next;
      _available = info;
      _lastCheckedAt = DateTime.now();
    } else if (_status == UpdateStatus.unknown) {
      _lastCheckedAt = DateTime.now(); // no result yet: report the attempt
    }
    _checking = false;
    // Only the version strings ever reach the log — no user content.
    DiagnosticsLog.instance.log(
      'update',
      'check: ${next?.name ?? 'failed'}'
          '${info == null ? '' : ' latest=${info.version}'}',
    );
    notifyListeners();
  }

  /// Parses a manifest body into an [UpdateInfo], or null when it isn't one.
  /// Accepts the plain `{latest, url, notes}` shape and GitHub's
  /// releases/latest shape (`tag_name` / `html_url` / `body`). The URL must
  /// be https — a tampered or downgraded manifest must not be able to point
  /// users at a plaintext download page, let alone a non-web URL.
  ///
  /// `html_url` WINS over `url` when both are present: a real GitHub release
  /// object carries both, and its `url` is the api.github.com resource
  /// endpoint — opening that from "Open Download Page" lands the user on raw
  /// JSON with no installer in sight. A hand-hosted manifest has no
  /// `html_url`, so it still falls through to `url`.
  @visibleForTesting
  static UpdateInfo? parseManifest(String body) {
    try {
      final decoded = jsonDecode(body);
      if (decoded is! Map<String, dynamic>) return null;
      final Object? version = decoded['latest'] ?? decoded['tag_name'];
      final Object? url = decoded['html_url'] ?? decoded['url'];
      final Object? notes = decoded['notes'] ?? decoded['body'];
      if (version is! String || version.trim().isEmpty) return null;
      if (url is! String) return null;
      final uri = Uri.tryParse(url.trim());
      if (uri == null || uri.scheme != 'https') {
        return null;
      }
      return UpdateInfo(
        version: version.trim().replaceFirst(RegExp('^[vV]'), ''),
        url: url.trim(),
        notes: notes is String ? notes.trim() : '',
      );
    } catch (_) {
      return null;
    }
  }

  /// True when [candidate] is a strictly newer dotted-numeric version than
  /// [current]. Tolerates a leading `v` and a `+build` suffix; anything not
  /// purely numeric-dotted (e.g. `1.1.0-rc1`) is conservatively NOT newer, so
  /// a malformed manifest can never raise the banner.
  ///
  /// The CURRENT build alone may carry a `-prerelease` suffix — release.yml
  /// ships `-` tags as prereleases, and an RC install whose own version was
  /// unparseable read "up to date" forever. It compares by its release core,
  /// and the GA of that same core counts as newer (semver: X.Y.Z-pre <
  /// X.Y.Z). A prerelease candidate still never parses, so none is offered.
  @visibleForTesting
  static bool isNewer(String candidate, String current) {
    final a = _parseVersion(candidate);
    final plus = current.indexOf('+'); // a '-' in +build metadata isn't one
    final core = plus >= 0 ? current.substring(0, plus) : current;
    final dash = core.indexOf('-');
    final b = _parseVersion(dash >= 0 ? core.substring(0, dash) : core);
    if (a == null || b == null) return false;
    final len = a.length > b.length ? a.length : b.length;
    for (var i = 0; i < len; i++) {
      final x = i < a.length ? a[i] : 0;
      final y = i < b.length ? b[i] : 0;
      if (x != y) return x > y;
    }
    return dash >= 0; // equal cores: the GA beats its own prerelease
  }

  static List<int>? _parseVersion(String raw) {
    var s = raw.trim();
    if (s.startsWith('v') || s.startsWith('V')) s = s.substring(1);
    final plus = s.indexOf('+');
    if (plus >= 0) s = s.substring(0, plus);
    if (s.isEmpty) return null;
    final parts = <int>[];
    for (final part in s.split('.')) {
      final n = int.tryParse(part);
      if (n == null || n < 0) return null;
      parts.add(n);
    }
    return parts;
  }

  /// The production fetcher: plain dart:io HttpClient (no extra dependency),
  /// bounded by timeouts so a wedged connection can't hold resources.
  static Future<String> _fetchOverHttp(Uri url) async {
    const timeout = Duration(seconds: 10);
    final client = HttpClient()..connectionTimeout = timeout;
    try {
      final request = await client.getUrl(url).timeout(timeout);
      // GitHub's API rejects requests without a User-Agent.
      request.headers.set(HttpHeaders.userAgentHeader, 'vocara-update-check');
      request.headers.set(HttpHeaders.acceptHeader, 'application/json');
      final response = await request.close().timeout(timeout);
      if (response.statusCode != HttpStatus.ok) {
        throw HttpException('status ${response.statusCode}', uri: url);
      }
      return await response.transform(utf8.decoder).join().timeout(timeout);
    } finally {
      client.close(force: true);
    }
  }
}
