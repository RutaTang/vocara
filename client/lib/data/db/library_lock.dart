import 'dart:io';

import 'package:path/path.dart' as p;

import '../services/diagnostics_log.dart';

/// The single-instance guard for one library directory: an exclusive
/// advisory lock on `<dir>/vocara.lock`, held for the life of the process.
///
/// Boot recovery and every destructive boot sweep assume that nothing else is
/// writing the library: a staged WAV with no row is a crashed take, a pool
/// file with no row is a crash leftover, the staging trees hold nothing in
/// flight. A second Vocara process on the same library (`open -n`, or two
/// builds that share a bundle id) breaks all of that while the first one is
/// recording or saving: its boot adopts the first one's half-written take as
/// "(recovered)" and deletes the file, and the staging sweep trashes what
/// the first one has not saved yet. The migrator makes the same assumption.
/// Only the process holding this lock may run them.
///
/// The lock is a POSIX record lock ([RandomAccessFile.lock]), which belongs
/// to the PROCESS: a second request from this process on the same file
/// succeeds, and closing ANY descriptor this process holds on the file
/// drops them all. So the file is opened once per library, the handle is
/// kept in [_held] for the life of the isolate and never closed, and a later
/// open of the same library in this process (tests open several stores over
/// one directory) reuses it. The kernel releases the lock when the process
/// exits, however it exits.
class LibraryLock {
  LibraryLock._();

  /// The lock file's name inside the library directory.
  static const String fileName = 'vocara.lock';

  /// Open lock handles by canonical lock-file path. Never closed (see the
  /// class doc).
  static final Map<String, RandomAccessFile> _held = {};

  /// Errno values a non-blocking lock attempt fails with when another
  /// process holds the lock: EACCES and EAGAIN (11 on Linux, 35 on macOS).
  static const Set<int> _heldElsewhere = {13, 11, 35};

  /// Takes the lock for [dir], or confirms this process already holds it.
  /// Returns false only when another process holds it.
  ///
  /// A failure that says nothing about another process (the file cannot be
  /// opened, the filesystem does not support locks) returns true, so the
  /// app behaves exactly as it did before the guard existed rather than
  /// never running crash recovery on such a volume again.
  static Future<bool> acquire(Directory dir) async {
    final path = p.join(p.canonicalize(dir.path), fileName);
    // A handle for a lock file that has since been deleted (a test tearing
    // its directory down and making a new one at the same path) guards
    // nothing: take the new file's lock.
    if (_held.containsKey(path) && await File(path).exists()) return true;
    RandomAccessFile? raf;
    try {
      raf = await File(path).open(mode: FileMode.append);
      await raf.lock(FileLock.exclusive);
      _held[path] = raf;
      return true;
    } on FileSystemException catch (e) {
      // Closing is safe: this process holds no lock on the file (it would
      // have returned above), so there is nothing for the close to drop.
      try {
        await raf?.close();
      } catch (_) {}
      final code = e.osError?.errorCode;
      if (raf != null && code != null && _heldElsewhere.contains(code)) {
        return false;
      }
      DiagnosticsLog.instance.log(
        'persistence',
        'library lock unavailable (${e.osError?.errorCode}); '
            'proceeding without the single-instance guard',
      );
      return true;
    }
  }
}
