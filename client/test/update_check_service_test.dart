// UpdateCheckService: manifest parsing (plain + GitHub release shapes),
// version comparison, the never-fails policy on every failure mode, and the
// once-per-run auto-check. All network is replaced by an injected fetcher.

import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/update_check_service.dart';

void main() {
  group('parseManifest', () {
    test('reads the plain {latest, url, notes} shape', () {
      final info = UpdateCheckService.parseManifest(
        '{"latest": "1.2.0", "url": "https://example.com/dl", '
        '"notes": "Fixes."}',
      );
      expect(info, isNotNull);
      expect(info!.version, '1.2.0');
      expect(info.url, 'https://example.com/dl');
      expect(info.notes, 'Fixes.');
    });

    test('reads the GitHub releases/latest shape and strips the v prefix', () {
      final info = UpdateCheckService.parseManifest(
        '{"tag_name": "v1.2.0", '
        '"html_url": "https://github.com/RutaTang/vocara/releases/tag/v1.2.0",'
        ' "body": "Release notes"}',
      );
      expect(info, isNotNull);
      expect(info!.version, '1.2.0');
      expect(info.url, contains('/releases/tag/v1.2.0'));
      expect(info.notes, 'Release notes');
    });

    test('a real GitHub release carries BOTH urls; the human page wins', () {
      // The shape the shipping manifestUrl actually returns. `url` is the
      // api.github.com resource endpoint, and the banner's button opens
      // whatever lands here — preferring it showed the user raw JSON.
      final info = UpdateCheckService.parseManifest(
        '{"tag_name": "v1.2.0", '
        '"url": "https://api.github.com/repos/RutaTang/vocara/releases/123", '
        '"html_url": "https://github.com/RutaTang/vocara/releases/tag/v1.2.0",'
        ' "body": "Release notes"}',
      );
      expect(
        info!.url,
        'https://github.com/RutaTang/vocara/releases/tag/v1.2.0',
      );
      expect(info.url, isNot(contains('api.github.com')));
    });

    test('missing notes default to empty', () {
      final info = UpdateCheckService.parseManifest(
        '{"latest": "1.2.0", "url": "https://example.com"}',
      );
      expect(info!.notes, isEmpty);
    });

    test('rejects malformed JSON, non-objects and missing fields', () {
      expect(UpdateCheckService.parseManifest('not json'), isNull);
      expect(UpdateCheckService.parseManifest('[1, 2]'), isNull);
      expect(UpdateCheckService.parseManifest('{"url": "https://x"}'), isNull);
      expect(UpdateCheckService.parseManifest('{"latest": "1.0"}'), isNull);
      expect(
        UpdateCheckService.parseManifest('{"latest": "", "url": "https://x"}'),
        isNull,
      );
    });

    test('rejects non-https URLs so a manifest cannot open anything else', () {
      expect(
        UpdateCheckService.parseManifest(
          '{"latest": "9.0.0", "url": "file:///Applications"}',
        ),
        isNull,
      );
      expect(
        UpdateCheckService.parseManifest(
          '{"latest": "9.0.0", "url": "/usr/bin/say"}',
        ),
        isNull,
      );
      expect(
        UpdateCheckService.parseManifest(
          '{"latest": "9.0.0", "url": "http://example.com/dl"}',
        ),
        isNull,
      );
    });
  });

  group('isNewer', () {
    test('strictly newer dotted versions win', () {
      expect(UpdateCheckService.isNewer('1.0.1', '1.0.0'), isTrue);
      expect(UpdateCheckService.isNewer('1.1', '1.0.9'), isTrue);
      expect(UpdateCheckService.isNewer('2.0.0', '1.9.9'), isTrue);
      // Numeric, not lexicographic.
      expect(UpdateCheckService.isNewer('1.0.10', '1.0.9'), isTrue);
    });

    test('equal or older is not newer', () {
      expect(UpdateCheckService.isNewer('1.0.0', '1.0.0'), isFalse);
      expect(UpdateCheckService.isNewer('1.0', '1.0.0'), isFalse);
      expect(UpdateCheckService.isNewer('0.9.9', '1.0.0'), isFalse);
    });

    test('tolerates v prefix and +build suffix', () {
      expect(UpdateCheckService.isNewer('v1.0.1', '1.0.0'), isTrue);
      expect(UpdateCheckService.isNewer('1.0.1+7', '1.0.0'), isTrue);
      expect(UpdateCheckService.isNewer('v1.0.0', '1.0.0'), isFalse);
    });

    test('unparseable versions are conservatively not newer', () {
      expect(UpdateCheckService.isNewer('abc', '1.0.0'), isFalse);
      expect(UpdateCheckService.isNewer('1.0.0-rc1', '1.0.0'), isFalse);
      expect(UpdateCheckService.isNewer('', '1.0.0'), isFalse);
      expect(UpdateCheckService.isNewer('1.0.1', 'garbage'), isFalse);
    });

    test('a prerelease CURRENT build is offered its GA and anything later', () {
      // release.yml ships `-` tags as prereleases; an RC install's own
      // version used to be unparseable, so it read "up to date" forever.
      expect(UpdateCheckService.isNewer('1.0.0', '1.0.0-rc.1'), isTrue);
      expect(UpdateCheckService.isNewer('1.0.1', 'v1.0.0-rc.1+3'), isTrue);
      expect(UpdateCheckService.isNewer('0.9.9', '1.0.0-rc.1'), isFalse);
      // A prerelease candidate is still never offered.
      expect(UpdateCheckService.isNewer('1.0.0-rc.2', '1.0.0-rc.1'), isFalse);
      // A '-' inside +build metadata is not a prerelease marker.
      expect(UpdateCheckService.isNewer('1.0.0', '1.0.0+build-7'), isFalse);
    });
  });

  group('check', () {
    test('newer manifest -> updateAvailable with the release info', () async {
      final service = UpdateCheckService(
        fetch: (_) async =>
            '{"latest": "9.9.9", "url": "https://example.com/dl", '
            '"notes": "Big"}',
        currentVersion: '1.0.0',
      );
      await service.check();
      expect(service.status, UpdateStatus.updateAvailable);
      expect(service.available!.version, '9.9.9');
      expect(service.lastCheckedAt, isNotNull);
    });

    test('same version -> upToDate with no available info', () async {
      final service = UpdateCheckService(
        fetch: (_) async => '{"latest": "1.0.0", "url": "https://example.com"}',
        currentVersion: '1.0.0',
      );
      await service.check();
      expect(service.status, UpdateStatus.upToDate);
      expect(service.available, isNull);
    });

    test('a throwing fetcher (offline / 404) degrades to unknown', () async {
      final service = UpdateCheckService(
        fetch: (_) async => throw const SocketException('offline'),
        currentVersion: '1.0.0',
      );
      await service.check();
      expect(service.status, UpdateStatus.unknown);
      expect(service.available, isNull);
      expect(service.lastCheckedAt, isNotNull);
    });

    test('a malformed body degrades to unknown', () async {
      final service = UpdateCheckService(
        fetch: (_) async => '<html>rate limited</html>',
        currentVersion: '1.0.0',
      );
      await service.check();
      expect(service.status, UpdateStatus.unknown);
    });

    test('a later successful check replaces an unknown result', () async {
      var fail = true;
      final service = UpdateCheckService(
        fetch: (_) async {
          if (fail) throw const SocketException('offline');
          return '{"latest": "2.0.0", "url": "https://example.com"}';
        },
        currentVersion: '1.0.0',
      );
      await service.check();
      expect(service.status, UpdateStatus.unknown);
      fail = false;
      await service.check();
      expect(service.status, UpdateStatus.updateAvailable);
    });

    test('a failed check keeps the last good result', () async {
      // A manual check made offline (or past GitHub's hourly limit) erased an
      // update the auto-check had found: the banner went, and Settings said
      // the server could not be reached.
      var fail = false;
      final service = UpdateCheckService(
        fetch: (_) async {
          if (fail) throw const SocketException('offline');
          return '{"latest": "2.0.0", "url": "https://example.com"}';
        },
        currentVersion: '1.0.0',
      );
      await service.check();
      expect(service.status, UpdateStatus.updateAvailable);
      final checkedAt = service.lastCheckedAt;
      fail = true;
      await service.check();
      expect(service.status, UpdateStatus.updateAvailable);
      expect(service.available?.version, '2.0.0');
      expect(service.lastCheckedAt, checkedAt);
    });

    test('a failed check after "up to date" says it failed', () async {
      // Kept, "up to date" read as a fresh answer: Settings said "You have the
      // latest version" for a check that never reached the server.
      var fail = false;
      final service = UpdateCheckService(
        fetch: (_) async {
          if (fail) throw const SocketException('offline');
          return '{"latest": "1.0.0", "url": "https://example.com"}';
        },
        currentVersion: '1.0.0',
      );
      await service.check();
      expect(service.status, UpdateStatus.upToDate);
      fail = true;
      await service.check();
      expect(service.status, UpdateStatus.unknown);
    });

    test('notifies listeners when the check starts and completes', () async {
      var notifications = 0;
      final service = UpdateCheckService(
        fetch: (_) async => '{"latest": "1.0.0", "url": "https://example.com"}',
        currentVersion: '1.0.0',
      )..addListener(() => notifications++);
      await service.check();
      expect(notifications, 2);
    });
  });

  group('autoCheck', () {
    test(
      'fetches exactly once per app run; manual check still works',
      () async {
        var fetches = 0;
        final service = UpdateCheckService(
          fetch: (_) async {
            fetches++;
            return '{"latest": "1.0.0", "url": "https://example.com"}';
          },
          currentVersion: '1.0.0',
        );
        await service.autoCheck();
        await service.autoCheck();
        await service.autoCheck();
        expect(fetches, 1);
        await service.check();
        expect(fetches, 2);
      },
    );
  });

  group('dismissNotice', () {
    test('sticks for the run and notifies once', () async {
      final service = UpdateCheckService(
        fetch: (_) async => '{"latest": "9.0.0", "url": "https://example.com"}',
        currentVersion: '1.0.0',
      );
      await service.check();
      expect(service.noticeDismissed, isFalse);
      var notifications = 0;
      service.addListener(() => notifications++);
      service.dismissNotice();
      service.dismissNotice();
      expect(service.noticeDismissed, isTrue);
      expect(notifications, 1);
    });
  });
}
