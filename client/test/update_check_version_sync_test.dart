// Pins UpdateCheckService's hard-coded version constants to pubspec.yaml.
//
// The release guard compares the git tag against pubspec.yaml only; nothing
// there sees the Dart constants. Without this test, tagging v0.2.0 with a
// stale `appVersion = '0.1.0'` ships an app that mis-identifies itself and
// shows a permanent "update available" banner for its own release. Release
// CI runs `flutter test`, so this drift now fails the pipeline instead.

import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/update_check_service.dart';

void main() {
  test('appVersion and appBuildNumber match pubspec.yaml', () {
    // flutter test runs with the package root as the working directory.
    final pubspec = File('pubspec.yaml').readAsStringSync();
    final match = RegExp(
      r'^version:\s*([0-9][^+\s]*)\+(\S+)\s*$',
      multiLine: true,
    ).firstMatch(pubspec);
    expect(match, isNotNull, reason: 'pubspec.yaml must declare version: X+B');
    expect(UpdateCheckService.appVersion, match!.group(1));
    expect(UpdateCheckService.appBuildNumber, match.group(2));
  });
}
