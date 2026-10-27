import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:path/path.dart' as p;
import 'package:path_provider/path_provider.dart';

import '../db/safe_migrator.dart';

/// App-wide diagnostics log: a small rotating plain-text log under the app
/// support directory, fed by the safety-critical catch sites (boot recovery,
/// sweeps, sync repairs) and by the global uncaught-error hooks.
///
/// Design constraints, in order:
///  * NEVER makes anything worse — every write is best-effort and swallowed;
///    a diagnostics failure must not take down the code path it observes.
///  * NO user content — callers log counts, error types and area tags, never
///    lyrics, titles, paths into user media, or audio. Because the global
///    hooks record raw exception strings (FileSystemException messages carry
///    full paths), [log] additionally scrubs absolute filesystem paths from
///    every entry — the promise is enforced here, not just asked of callers.
///  * Bounded — two files of [_defaultMaxBytesPerFile] each
///    (`diagnostics.log` + `diagnostics.log.1`), rotated by size.
///
/// [exportArchive] bundles the log files plus app version and the library's
/// schema `user_version` into one text file at a temp path, for the user to
/// attach to a bug report.
class DiagnosticsLog {
  DiagnosticsLog._() : _maxBytesPerFile = _defaultMaxBytesPerFile;

  /// An isolated instance for tests: logs under [directory] (bind later via
  /// [bindDirectory] when null) with an optional smaller rotation cap.
  @visibleForTesting
  DiagnosticsLog.forTesting({
    Directory? directory,
    int maxBytesPerFile = _defaultMaxBytesPerFile,
  }) : _maxBytesPerFile = maxBytesPerFile,
       _dir = directory;

  /// The single app-wide log (all areas share one file, so ordering across
  /// subsystems is preserved).
  static final DiagnosticsLog instance = DiagnosticsLog._();

  static const int _defaultMaxBytesPerFile = 512 * 1024;
  static const String _fileName = 'diagnostics.log';

  /// Entries buffered in memory before a directory is bound (early boot).
  static const int _maxPendingEntries = 500;

  /// One entry is capped so a pathological error string can't blow through
  /// the rotation budget in a single write.
  static const int _maxEntryChars = 4096;

  final int _maxBytesPerFile;
  Directory? _dir;
  bool _hooksAttached = false;
  final List<String> _pending = [];

  /// Serialized write queue: [log] is synchronous for callers, so appends are
  /// chained here to keep entries ordered and the file un-torn.
  Future<void> _tail = Future.value();

  /// Binds the on-disk destination and wires the global uncaught-error hooks
  /// (logged, then forwarded to whatever handler was installed before).
  /// Call once at the very top of `main()`; failures degrade to in-memory
  /// buffering and never throw.
  Future<void> attachGlobalHooks({Directory? directory}) async {
    try {
      bindDirectory(
        directory ?? _dir ?? await getApplicationSupportDirectory(),
      );
    } catch (_) {
      // No writable directory (headless test, broken platform channel): keep
      // buffering in memory; exportArchive still includes the buffer.
    }
    if (_hooksAttached) return;
    _hooksAttached = true;

    // Uncaught errors from the framework (build/layout/paint, gesture
    // callbacks). Log first, then hand off to the previous handler so the
    // default red-screen / console reporting is unchanged.
    final previousOnError = FlutterError.onError;
    FlutterError.onError = (details) {
      log('flutter', _describe(details.exception, details.stack));
      previousOnError?.call(details);
    };

    // Uncaught errors from the platform dispatcher (async callbacks outside
    // the framework). Preserve the previous handler's verdict; with no
    // previous handler, report "unhandled" exactly as before.
    final previousPlatformOnError = PlatformDispatcher.instance.onError;
    PlatformDispatcher.instance.onError = (error, stack) {
      log('platform', _describe(error, stack));
      return previousPlatformOnError?.call(error, stack) ?? false;
    };
  }

  /// Binds (or rebinds) the directory the log files live under and flushes
  /// any entries buffered before boot got this far.
  @visibleForTesting
  void bindDirectory(Directory directory) {
    _dir = directory;
    if (_pending.isEmpty) return;
    final backlog = List.of(_pending);
    _pending.clear();
    for (final entry in backlog) {
      _enqueue(() => _append(directory, entry));
    }
  }

  /// Appends one timestamped entry. Synchronous and infallible for the
  /// caller: the write is queued (ordered) and any IO failure is swallowed.
  /// Absolute filesystem paths in [message] are scrubbed before anything is
  /// buffered or written.
  void log(String area, String message) {
    var entry =
        '${DateTime.now().toUtc().toIso8601String()} '
        '[$area] ${scrubPaths(message)}';
    if (entry.length > _maxEntryChars) {
      entry = entry.substring(0, _maxEntryChars);
    }
    entry = '$entry\n';
    if (kDebugMode) debugPrint('[diagnostics] ${entry.trimRight()}');
    final dir = _dir;
    if (dir == null) {
      if (_pending.length < _maxPendingEntries) _pending.add(entry);
      return;
    }
    _enqueue(() => _append(dir, entry));
  }

  /// Completes once every entry queued so far has been written (or failed).
  @visibleForTesting
  Future<void> flush() => _tail;

  /// Writes a single plain-text diagnostics bundle to a temp path and returns
  /// that path: app version + library schema `user_version` + the rotated log
  /// files, oldest first. NO user content and NO audio ever goes in — the log
  /// itself only carries what [log] callers wrote (areas, counts, error
  /// types).
  Future<String> exportArchive() async {
    await _tail; // the bundle reflects everything logged so far
    final buffer = StringBuffer()
      ..writeln('Vocara diagnostics bundle')
      ..writeln('generated: ${DateTime.now().toUtc().toIso8601String()}')
      ..writeln('app version: ${await _appVersion()}')
      ..writeln('schema user_version: ${await _schemaUserVersion()}')
      ..writeln();
    final dir = _dir;
    if (dir != null) {
      // Oldest first: the rolled file precedes the current one.
      for (final name in const ['$_fileName.1', _fileName]) {
        final f = File(p.join(dir.path, name));
        if (!await f.exists()) continue;
        // Scrubbed again on the way out: lines an older build wrote stay on
        // disk until rotation, written with whatever its scrubber missed.
        buffer
          ..writeln('===== $name =====')
          ..writeln(scrubPaths(await f.readAsString()));
      }
    }
    if (_pending.isNotEmpty) {
      buffer.writeln('===== buffered (no log directory bound) =====');
      _pending.forEach(buffer.write);
    }
    final tmp = await Directory.systemTemp.createTemp('vocara-diagnostics-');
    final out = File(p.join(tmp.path, 'vocara-diagnostics.txt'));
    try {
      await out.writeAsString(buffer.toString(), flush: true);
    } catch (_) {
      // A write that fails (a full disk) hands the caller no path to discard:
      // the folder, and whatever part of the bundle landed, are ours to remove.
      try {
        await tmp.delete(recursive: true);
      } catch (_) {}
      rethrow;
    }
    return out.path;
  }

  /// Removes a bundle [exportArchive] wrote, once the caller has copied it
  /// where the user wanted it: the file, then its folder ONLY if that left it
  /// empty (a non-recursive delete, so a path from anywhere else can never take
  /// more than the file with it). Every export used to leave both behind.
  /// Best-effort; never throws.
  static Future<void> discardArchive(String path) async {
    final file = File(path);
    try {
      await file.delete();
    } catch (_) {}
    try {
      await file.parent.delete();
    } catch (_) {
      // Not empty, or already gone: the file was all that was ours.
    }
  }

  void _enqueue(Future<void> Function() write) {
    _tail = _tail.then((_) async {
      try {
        await write();
      } catch (_) {
        // Diagnostics must never become a failure source of its own.
      }
    });
  }

  Future<void> _append(Directory dir, String entry) async {
    final file = File(p.join(dir.path, _fileName));
    await _rotateIfNeeded(file, entry.length);
    // flush: true — an entry usually describes a failure, and the process may
    // be about to die; a buffered-and-lost diagnostic is worthless.
    await file.writeAsString(entry, mode: FileMode.append, flush: true);
  }

  /// Size-based rotation: when the incoming entry would push the current file
  /// past the cap, the current file becomes `.1` (replacing the previous one)
  /// and a fresh file starts — exactly two files ever exist.
  Future<void> _rotateIfNeeded(File file, int incomingLength) async {
    final length = await file.exists() ? await file.length() : 0;
    if (length == 0 || length + incomingLength <= _maxBytesPerFile) return;
    final rolled = File('${file.path}.1');
    if (await rolled.exists()) await rolled.delete();
    await file.rename(rolled.path);
  }

  static String _describe(Object error, StackTrace? stack) {
    if (stack == null) return '$error';
    // The first frames identify the failure; the entry cap bounds the rest.
    final head = '$stack'.split('\n').take(8).join('\n');
    return '$error\n$head';
  }

  /// Replaces absolute filesystem paths with a `<path>` placeholder (the
  /// extension survives, it identifies the file kind without naming it).
  /// Anchored to user-space roots so `package:` URIs in stack frames — the
  /// most valuable diagnostic content — pass through untouched. Paths may
  /// contain spaces (song titles do), so a match runs greedily to the next
  /// quote or line end and is then cut back at the last extension in the
  /// path's final component; when in doubt this over-scrubs rather than leak.
  @visibleForTesting
  static String scrubPaths(String message) {
    return message.replaceAllMapped(_absolutePath, (m) {
      final matched = m[0]!;
      // The path runs to where a second root starts a new word (prose naming
      // another path), else to the match end — and only its FINAL component
      // can carry the extension: a dot further up ('Vol.2 Live/…') belongs to
      // a directory name, and cutting there leaked the rest of the path.
      final end = _nextRoot.firstMatch(matched)?.start ?? matched.length;
      final last = matched.lastIndexOf('/', end - 1) + 1;
      // Within it, the LAST extension-looking run wins (a dotted file name,
      // 'Ms.Lauryn Hill.wav', must not leak its tail) — but a file type the
      // app actually handles wins over later runs that read as NUMBERS, so
      // 'take.wav after 12.3s' keeps its '.wav' and the context. Any other
      // later run is still part of the name ('Lesson.mov with Jane Doe.avi').
      Match? ext;
      Match? known;
      for (final e in _extensionInside.allMatches(matched, last)) {
        if (e.start >= end) break;
        if (known != null && !_numericRun.hasMatch(e[0]!)) known = null;
        ext = e;
        if (_knownExtensions.contains(e[0]!.substring(1).toLowerCase())) {
          known = e;
        }
      }
      ext = known ?? ext;
      // Text after the cut is prose, not path — but it may hold another path
      // of its own, so scrub it recursively (it is strictly shorter).
      if (ext == null) return '<path>${scrubPaths(matched.substring(end))}';
      return '<path>${ext[0]}${scrubPaths(matched.substring(ext.end))}';
    });
  }

  static const String _roots =
      '/(?:Users|private|var|tmp|Volumes|Library|Applications|home|etc|opt)';
  // An apostrophe ends the path only where it reads as the closing quote of
  // Dart's `path = '…'` (line end, punctuation, or ` (OS Error`); inside a
  // name ("Don't Stop.m4a", "Guns N' Roses/…", "The Players' (Club Mix)") it
  // is part of the path, and stopping there leaked the rest of the name
  // unscrubbed. Any ` (` used to count, the last case included. A double
  // quote reads the same way: in a name ('Blue Monday (12" Version).mp3',
  // 'Song "Live".m4a') it is part of the path, and the scrub stopped at it.
  static final RegExp _absolutePath = RegExp(
    '$_roots(?:/(?:[^\'"\\n]|[\'"](?!\\s*\$|\\s+\\(OS Error|\\n|[,;:)\\]}.]))+)+',
  );
  static final RegExp _nextRoot = RegExp('\\s[\'"]?(?=$_roots/)');
  static final Set<String> _knownExtensions = {
    ...'wav wave aif aiff aifc caf mp3 m4a aac flac ogg oga opus mp4 m4v mov '
            'webm mkv json db sqlite sqlite3 ckpt pth onnx yaml txt log zip '
            'tmp part png jpg jpeg pdf py'
        .split(' '),
  };
  static final RegExp _numericRun = RegExp(r'^\.\d');
  static final RegExp _extensionInside = RegExp(
    r"""\.[A-Za-z0-9]{1,8}(?=[\s.,;:)\]}'"]|$)""",
  );

  /// The running bundle's version (CFBundleShortVersionString + build +
  /// bundle id), read from the app bundle's own Info.plist. 'unknown' outside
  /// a bundled build (tests).
  Future<String> _appVersion() async {
    try {
      // <App>.app/Contents/MacOS/<exe> -> <App>.app/Contents/Info.plist
      final exe = File(Platform.resolvedExecutable);
      final plist = File(p.join(exe.parent.parent.path, 'Info.plist'));
      if (!await plist.exists()) return 'unknown';
      // Built macOS Info.plists stay XML unless Xcode is told otherwise; fall
      // back to `defaults read` if the regex ever stops matching.
      final xml = await plist.readAsString();
      String? read(String key) => RegExp(
        '<key>$key</key>\\s*<string>([^<]*)</string>',
      ).firstMatch(xml)?.group(1);
      var version = read('CFBundleShortVersionString');
      if (version == null) {
        final result = await Process.run('/usr/bin/defaults', [
          'read',
          p.withoutExtension(plist.path),
          'CFBundleShortVersionString',
        ]);
        if (result.exitCode == 0) version = (result.stdout as String).trim();
      }
      if (version == null || version.isEmpty) return 'unknown';
      final build = read('CFBundleVersion') ?? '';
      final bundleId = read('CFBundleIdentifier') ?? '';
      return [
        version,
        if (build.isNotEmpty) '($build)',
        if (bundleId.isNotEmpty) bundleId,
      ].join(' ');
    } catch (_) {
      return 'unknown';
    }
  }

  /// The library database's schema `user_version`, read without opening the
  /// store for writing (and without ever creating a file).
  Future<String> _schemaUserVersion() async {
    try {
      final dir = _dir ?? await getApplicationSupportDirectory();
      final dbPath = p.join(dir.path, 'vocara.db');
      if (!File(dbPath).existsSync()) return 'no database file';
      return SafeMigrator().readUserVersion(dbPath).toString();
    } catch (e) {
      return 'unreadable (${e.runtimeType})';
    }
  }
}
