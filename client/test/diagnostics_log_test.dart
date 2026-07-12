// The rotating diagnostics log: ordered on-disk entries, a strict two-file
// size budget, pre-bind buffering, global hook forwarding, and the export
// bundle (log files + version metadata, never user content).

import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/services/diagnostics_log.dart';

void main() {
  late Directory dir;

  setUp(() async {
    dir = await Directory.systemTemp.createTemp('vocara_diag');
  });

  tearDown(() async {
    if (await dir.exists()) await dir.delete(recursive: true);
  });

  File currentFile() => File(p.join(dir.path, 'diagnostics.log'));
  File rolledFile() => File(p.join(dir.path, 'diagnostics.log.1'));

  test('log appends timestamped entries in order', () async {
    final log = DiagnosticsLog.forTesting(directory: dir);
    log.log('boot', 'first message');
    log.log('persistence', 'second message');
    await log.flush();

    final content = await currentFile().readAsString();
    expect(content, contains('[boot] first message'));
    expect(content, contains('[persistence] second message'));
    expect(
      content.indexOf('first message'),
      lessThan(content.indexOf('second message')),
    );
    // Every line starts with a UTC ISO-8601 timestamp.
    for (final line in content.trim().split('\n')) {
      expect(line, matches(r'^\d{4}-\d{2}-\d{2}T[\d:.]+Z \['));
    }
  });

  test('rotates into exactly two files at the size cap, newest last', () async {
    final log = DiagnosticsLog.forTesting(directory: dir, maxBytesPerFile: 400);
    for (var i = 0; i < 40; i++) {
      log.log('rotation', 'entry $i ${'x' * 40}');
    }
    await log.flush();

    expect(await rolledFile().exists(), isTrue);
    expect(await currentFile().length(), lessThanOrEqualTo(400));
    expect(await rolledFile().length(), lessThanOrEqualTo(400));
    // The budget is two files: nothing rotates beyond .1.
    expect(await File('${currentFile().path}.2').exists(), isFalse);
    // The newest entry lives in the current file.
    expect(await currentFile().readAsString(), contains('entry 39'));
  });

  test(
    'entries logged before a directory is bound are flushed after',
    () async {
      final log = DiagnosticsLog.forTesting();
      log.log('early', 'buffered before bind');
      log.bindDirectory(dir);
      await log.flush();

      expect(
        await currentFile().readAsString(),
        contains('[early] buffered before bind'),
      );
    },
  );

  test('attachGlobalHooks logs uncaught errors and forwards to the previous '
      'handlers', () async {
    final log = DiagnosticsLog.forTesting(directory: dir);
    final previousFlutter = FlutterError.onError;
    final previousPlatform = PlatformDispatcher.instance.onError;
    var flutterForwards = 0;
    var platformForwards = 0;
    FlutterError.onError = (details) => flutterForwards++;
    PlatformDispatcher.instance.onError = (error, stack) {
      platformForwards++;
      return true;
    };
    try {
      await log.attachGlobalHooks(directory: dir);

      FlutterError.reportError(
        FlutterErrorDetails(exception: StateError('boom widget')),
      );
      final verdict = PlatformDispatcher.instance.onError!(
        StateError('boom platform'),
        StackTrace.current,
      );

      // The previous handlers still ran, and their verdict is preserved.
      expect(flutterForwards, 1);
      expect(platformForwards, 1);
      expect(verdict, isTrue);

      await log.flush();
      final content = await currentFile().readAsString();
      expect(content, contains('boom widget'));
      expect(content, contains('boom platform'));
    } finally {
      FlutterError.onError = previousFlutter;
      PlatformDispatcher.instance.onError = previousPlatform;
    }
  });

  test('exportArchive bundles both log files with version metadata', () async {
    final log = DiagnosticsLog.forTesting(directory: dir, maxBytesPerFile: 400);
    for (var i = 0; i < 40; i++) {
      log.log('area', 'entry $i ${'x' * 40}');
    }

    final path = await log.exportArchive();
    addTearDown(() => File(path).parent.delete(recursive: true));

    final content = await File(path).readAsString();
    expect(content, contains('Vocara diagnostics bundle'));
    expect(content, contains('app version:'));
    expect(content, contains('schema user_version:'));
    // Both rotation generations are included, rolled (older) first.
    expect(content, contains('===== diagnostics.log.1 ====='));
    expect(content, contains('===== diagnostics.log ====='));
    expect(content, contains('entry 39'));
    expect(
      content.indexOf('===== diagnostics.log.1 ====='),
      lessThan(content.indexOf('===== diagnostics.log =====')),
    );
  });
}
