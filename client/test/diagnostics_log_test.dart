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

  group('scrubPaths', () {
    test('strips absolute user-space paths, keeping only the extension', () {
      expect(
        DiagnosticsLog.scrubPaths(
          'FileSystemException: Cannot open file, path = '
          "'/Users/ruta/Music/My Song Title.wav' (OS Error: No such file)",
        ),
        'FileSystemException: Cannot open file, path = '
        "'<path>.wav' (OS Error: No such file)",
      );
      expect(
        DiagnosticsLog.scrubPaths(
          'copy failed: /private/var/folders/ab/T/vocara/audio/01ABC/take.wav',
        ),
        'copy failed: <path>.wav',
      );
      expect(
        DiagnosticsLog.scrubPaths('/Volumes/USB/session (final)/mix.aiff gone'),
        '<path>.aiff gone',
      );
      // Prose after one path can hold another path; both are scrubbed.
      expect(
        DiagnosticsLog.scrubPaths('cp /Users/a/x.wav /Users/b/y.wav failed'),
        'cp <path>.wav <path>.wav failed',
      );
    });

    test('a dot in a directory or file name does not end the scrub early', () {
      // The cut used to land on the FIRST `.ext`-looking run, so a dotted
      // directory leaked the rest of the path verbatim.
      expect(
        DiagnosticsLog.scrubPaths(
          "path = '/Users/alice/Music/Vol.2 Live/My Secret Song.wav'",
        ),
        "path = '<path>.wav'",
      );
      expect(
        DiagnosticsLog.scrubPaths(
          '/Users/alice/feat.Ed Sheeran/Perfect Duet.wav (OS Error)',
        ),
        '<path>.wav (OS Error)',
      );
      expect(
        DiagnosticsLog.scrubPaths('at /Users/alice/Adele 2016.12 Remix/Hello'),
        'at <path>',
      );
      expect(
        DiagnosticsLog.scrubPaths('/Users/a/My.Song.wav gone'),
        '<path>.wav gone',
      );
      expect(
        DiagnosticsLog.scrubPaths("'/Users/a/Ms.Lauryn Hill.wav' gone"),
        "'<path>.wav' gone",
      );
      // A second path after an extensionless one is still scrubbed on its own.
      expect(
        DiagnosticsLog.scrubPaths('cp /Users/a/dir /Users/b/y.wav failed'),
        'cp <path> <path>.wav failed',
      );
    });

    test('a dotted number in the prose after a path is not taken for its '
        'extension', () {
      // Taking the LAST extension-looking run in the final component cut
      // the message at '12.3s', losing the file type and the context.
      expect(
        DiagnosticsLog.scrubPaths(
          'decode failed for /Users/alice/Music/take.wav after 12.3s',
        ),
        'decode failed for <path>.wav after 12.3s',
      );
      expect(
        DiagnosticsLog.scrubPaths(
          'stage /Users/alice/Music/My Song.m4a: rate 22.05 kHz unsupported',
        ),
        'stage <path>.m4a: rate 22.05 kHz unsupported',
      );
      // But only over numbers: a known type INSIDE a name whose own
      // extension is unlisted must not leak the rest of that name.
      for (final message in [
        'import failed: /Users/alice/Music/Sonata Op.27.Part II (Jane Doe).wma',
        'decode failed: /Users/alice/Movies/Lesson.mov with Jane Doe.avi',
      ]) {
        final out = DiagnosticsLog.scrubPaths(message);
        expect(out, isNot(contains('Jane Doe')), reason: out);
      }
    });

    test('leaves package URIs and non-path text untouched', () {
      const stackFrame =
          '#0  AssetStore._importFile '
          '(package:vocara/data/db/asset_store.dart:61:5)';
      expect(DiagnosticsLog.scrubPaths(stackFrame), stackFrame);
      expect(
        DiagnosticsLog.scrubPaths('ratio 44100/48000'),
        'ratio 44100/48000',
      );
      expect(DiagnosticsLog.scrubPaths('dart:async boom'), 'dart:async boom');
    });

    test(
      'log entries written through the hooks carry no absolute paths',
      () async {
        final log = DiagnosticsLog.forTesting(directory: dir);
        log.log(
          'mic',
          'discard failed: FileSystemException: Deletion failed, path = '
              "'/Users/ruta/Library/Containers/app/take.wav'",
        );
        await log.flush();
        final content = await currentFile().readAsString();
        expect(content, isNot(contains('/Users')));
        expect(content, contains('<path>.wav'));
      },
    );
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
