// Writes diagnostics_scrub.txt: what the Flutter app's
// DiagnosticsLog.scrubPaths makes of a set of messages, for the Rust port's
// hand-written matcher to reproduce exactly. One JSON line each:
// {"in": message, "out": scrubbed}.
//
// The scrubber below is copied verbatim from
// client/lib/data/services/diagnostics_log.dart (as of commit 00858ff): that
// file pulls in path_provider and the SQLite migrator, which a plain Dart VM
// script cannot load. Run: dart run diagnostics_scrub.gen.dart
import 'dart:convert';

class DiagnosticsLog {
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
  // A dot after a quote ends the path only as sentence punctuation; followed
  // by a letter or digit it begins the name's extension.
  static final RegExp _absolutePath = RegExp(
    '$_roots(?:/(?:[^\'"\\n]|[\'"](?!\\s*\$|\\s+\\(OS Error|\\n|[,;:)\\]}]|\\.(?![A-Za-z0-9])))+)+',
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

}

const messages = [
  "FileSystemException: Cannot open file, path = '/Users/ruta/Music/My Song Title.wav' (OS Error: No such file)",
  'copy failed: /private/var/folders/ab/T/vocara/audio/01ABC/take.wav',
  '/Volumes/USB/session (final)/mix.aiff gone',
  'cp /Users/a/x.wav /Users/b/y.wav failed',
  "path = '/Users/alice/Music/Vol.2 Live/My Secret Song.wav'",
  '/Users/alice/feat.Ed Sheeran/Perfect Duet.wav (OS Error)',
  'at /Users/alice/Adele 2016.12 Remix/Hello',
  '/Users/a/My.Song.wav gone',
  "'/Users/a/Ms.Lauryn Hill.wav' gone",
  'cp /Users/a/dir /Users/b/y.wav failed',
  "FileSystemException: Cannot open file, path = '/Users/a/Music/Don't Stop Believin.m4a' (OS Error: x, errno = 2)",
  "path = '/Users/a/Guns N' Roses/Sweet Child O' Mine.mp3' (OS Error)",
  "path = '/Users/a/Rock 'n' Roll/Take 2.wav'",
  "copy '/Users/a/x.wav' to '/Users/a/O' Mine.wav', failed",
  "path = '/Users/a/The Players' (Club Mix) - Mine.wav' (OS Error: x)",
  "open '/Users/a/Don't.wav', retrying",
  "copy '/Users/a/x.wav' to '/Users/b/y.aiff'",
  "path = '/Users/a/Music/Blue Monday (12\" Version).mp3' (OS Error: x, errno = 2)",
  "path = '/Users/a/Song \"Live at Wembley\".m4a'",
  "path = '/Users/a/Song \"Live\".m4a' (OS Error: x)",
  'open "/Users/a/x.wav" failed',
  'copy "/Users/a/x.wav" to "/Users/b/y.aiff"',
  'decode failed for /Users/alice/Music/take.wav after 12.3s',
  'stage /Users/alice/Music/My Song.m4a: rate 22.05 kHz unsupported',
  'import failed: /Users/alice/Music/Sonata Op.27.Part II (Jane Doe).wma',
  'decode failed: /Users/alice/Movies/Lesson.mov with Jane Doe.avi',
  '#0  AssetStore._importFile (package:vocara/data/db/asset_store.dart:61:5)',
  'ratio 44100/48000',
  'dart:async boom',
  "discard failed: FileSystemException: Deletion failed, path = '/Users/ruta/Library/Containers/app/take.wav'",
  '/Users',
  '/Users/',
  '/Users/x',
  '/Usersx/y',
  'x/Users/y.wav',
  '/variable/x.wav /var/log/system.log',
  '/home/me/.config/vocara/settings.json\nnext line /tmp/a.b.c.d',
  '/opt/homebrew/lib/python3.12/site-packages/demucs/model.ckpt loaded',
  '/etc/hosts;/etc/passwd',
  "/tmp/'quoted'/file.txt",
  "/tmp/a' (OS Error: denied)",
  "/tmp/a'  ",
  "/tmp/a'.",
  "/tmp/a'.x",
  "/tmp/a',b",
  '/Library/Application Support/com.rutatang.vocara/vocara.db-wal is locked',
  '/Applications/Vocara.app/Contents/MacOS/Vocara crashed at 0x1f',
  '/Users/a/b.c.d.e.f.g.h.i.j.k',
  '/Users/a/song.WAV and /Users/b/other.Flac',
  '/Users/a/x.wav /Users/b/y.wav',
  '/Users/a/x.wav "/Users/b/y.wav"',
  '/Users/a/été 中文 \u{1F600}.mp3 gone',
  '/Users/a/archive.tar.gz copied 1.5 MB in 0.25s',
  '/Users/a/take.wav after 12.3s then.mp3',
  '/Users/a/take.wav x.1 y.2 z.wav',
  '/Users/a/file.toolongextension done',
  '/Users/a/v1.2.3/ok',
  'no paths here at all',
  '',
];

void main() {
  for (final m in messages) {
    print(jsonEncode({'in': m, 'out': DiagnosticsLog.scrubPaths(m)}));
  }
}
