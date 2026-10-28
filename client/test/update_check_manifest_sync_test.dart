// Pins UpdateCheckService.manifestUrl to the public releases repo the
// release workflow publishes to.
//
// The two live in different files with nothing else tying them together: a
// repo renamed in one place only ships apps that ask a repo the workflow
// never publishes to, and every check reports "could not reach the update
// server" with no release-time signal. Release CI runs `flutter test`, so
// the drift fails the pipeline instead.

import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/update_check_service.dart';

void main() {
  test('manifestUrl names the manifest release.yml publishes', () {
    // flutter test runs with the package root as the working directory.
    final workflow = File(
      '../.github/workflows/release.yml',
    ).readAsStringSync();
    final repo = RegExp(
      r'^\s*RELEASES_REPO:\s*(\S+)\s*$',
      multiLine: true,
    ).firstMatch(workflow);
    expect(repo, isNotNull, reason: 'release.yml must set RELEASES_REPO');
    expect(
      UpdateCheckService.manifestUrl,
      'https://github.com/${repo!.group(1)}/releases/latest/download/'
      'latest.json',
    );
    expect(workflow, contains('latest.json'));
  });
}
