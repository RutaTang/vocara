// The rotating diagnostics log: ordered on-disk entries, a strict two-file
// size budget, pre-bind buffering, global hook forwarding, and the export
// bundle (log files + version metadata, never user content).

import 'dart:convert';
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

  test('entries are still written when the write lock cannot be had', () async {
    // A lock file that would not open (or a volume refusing locks) dropped
    // every entry from then on.
    await Directory(p.join(dir.path, 'diagnostics.log.lock')).create();
    final log = DiagnosticsLog.forTesting(directory: dir);
    log.log('boot', 'still recorded');
    await log.flush();
    expect(await currentFile().readAsString(), contains('still recorded'));
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

    test('an apostrophe inside a name does not end the scrub early', () {
      // The match stopped at the first `'`, so everything after it in the
      // name was logged verbatim.
      expect(
        DiagnosticsLog.scrubPaths(
          'FileSystemException: Cannot open file, path = '
          "'/Users/a/Music/Don't Stop Believin.m4a' (OS Error: x, errno = 2)",
        ),
        'FileSystemException: Cannot open file, path = '
        "'<path>.m4a' (OS Error: x, errno = 2)",
      );
      for (final message in [
        "path = '/Users/a/Guns N' Roses/Sweet Child O' Mine.mp3' (OS Error)",
        "path = '/Users/a/Rock 'n' Roll/Take 2.wav'",
        "copy '/Users/a/x.wav' to '/Users/a/O' Mine.wav', failed",
        // Any ` (` after an apostrophe used to read as the closing quote.
        "path = '/Users/a/The Players' (Club Mix) - Mine.wav' (OS Error: x)",
      ]) {
        final out = DiagnosticsLog.scrubPaths(message);
        for (final leak in ['Roses', 'Mine', 'Roll', 'Take 2', 'Club Mix']) {
          expect(out, isNot(contains(leak)), reason: out);
        }
      }
      // The closing quote still ends the path, so what follows survives.
      expect(
        DiagnosticsLog.scrubPaths("open '/Users/a/Don't.wav', retrying"),
        "open '<path>.wav', retrying",
      );
      expect(
        DiagnosticsLog.scrubPaths("copy '/Users/a/x.wav' to '/Users/b/y.aiff'"),
        "copy '<path>.wav' to '<path>.aiff'",
      );
    });

    test('a double quote inside a name does not end the scrub early', () {
      // The match stopped at the first `"`, so the rest of the name was
      // logged verbatim: 'path = <path>" Version).mp3'.
      expect(
        DiagnosticsLog.scrubPaths(
          "path = '/Users/a/Music/Blue Monday (12\" Version).mp3' "
          '(OS Error: x, errno = 2)',
        ),
        "path = '<path>.mp3' (OS Error: x, errno = 2)",
      );
      final live = DiagnosticsLog.scrubPaths(
        "path = '/Users/a/Song \"Live at Wembley\".m4a'",
      );
      expect(live, isNot(contains('Wembley')), reason: live);
      // A quote right before the extension's dot is still inside the name.
      expect(
        DiagnosticsLog.scrubPaths(
          "path = '/Users/a/Song \"Live\".m4a' (OS Error: x)",
        ),
        "path = '<path>.m4a' (OS Error: x)",
      );
      // A double-quoted path still ends at its closing quote.
      expect(
        DiagnosticsLog.scrubPaths('open "/Users/a/x.wav" failed'),
        'open "<path>.wav" failed',
      );
      expect(
        DiagnosticsLog.scrubPaths('copy "/Users/a/x.wav" to "/Users/b/y.aiff"'),
        'copy "<path>.wav" to "<path>.aiff"',
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

  // Lines an older build wrote stay on disk until rotation, with whatever its
  // scrubber missed, and the export copied them as they were.
  test('exportArchive scrubs what is already on disk', () async {
    await currentFile().writeAsString(
      "2026-10-01T00:00:00Z [old] cannot open '/Users/jane/Music/Secret.wav'\n",
    );
    final log = DiagnosticsLog.forTesting(directory: dir);
    final path = await log.exportArchive();
    final bundle = await File(path).readAsString();
    await DiagnosticsLog.discardArchive(path);
    expect(bundle, isNot(contains('jane')));
    expect(bundle, isNot(contains('Secret')));
    expect(bundle, contains("cannot open '<path>.wav'"));
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

  test(
    'a bundle whose write fails leaves no temporary folder behind',
    () async {
      // A full disk: the write throws, the caller gets no path to discard, and
      // the folder (with whatever part of the bundle landed) stayed in tmp.
      final log = DiagnosticsLog.forTesting(directory: dir);
      final temp = Directory(p.join(dir.path, 'tmp'))..createSync();
      await IOOverrides.runWithIOOverrides(
        () => expectLater(
          log.exportArchive(),
          throwsA(isA<FileSystemException>()),
        ),
        _FailingBundleWrites(temp),
      );
      expect(temp.listSync(), isEmpty);
    },
  );

  test('discardArchive removes the bundle and its own temp folder, never '
      'a folder that holds anything else', () async {
    final log = DiagnosticsLog.forTesting(directory: dir);
    final path = await log.exportArchive();
    await DiagnosticsLog.discardArchive(path);
    expect(await File(path).exists(), isFalse);
    expect(await File(path).parent.exists(), isFalse);

    // A path somewhere else: only that file goes, its neighbours stay.
    final other = File('${dir.path}/bundle.txt')..writeAsStringSync('x');
    final neighbour = File('${dir.path}/keep.txt')..writeAsStringSync('y');
    await DiagnosticsLog.discardArchive(other.path);
    expect(await other.exists(), isFalse);
    expect(await neighbour.exists(), isTrue);
    expect(await dir.exists(), isTrue);
  });
}

/// Routes the system temp folder into the test's own, and fails the bundle's
/// write after part of it landed, like a disk filling up mid-write.
final class _FailingBundleWrites extends IOOverrides {
  _FailingBundleWrites(this.temp);

  final Directory temp;

  @override
  Directory getSystemTempDirectory() => temp;

  @override
  File createFile(String path) => path.endsWith('vocara-diagnostics.txt')
      ? _UnwritableFile(super.createFile(path))
      : super.createFile(path);
}

class _UnwritableFile extends Fake implements File {
  _UnwritableFile(this._real);

  final File _real;

  @override
  String get path => _real.path;

  @override
  Future<File> writeAsString(
    String contents, {
    FileMode mode = FileMode.write,
    Encoding encoding = utf8,
    bool flush = false,
  }) async {
    await _real.writeAsString('partial');
    throw const FileSystemException('No space left on device');
  }
}
