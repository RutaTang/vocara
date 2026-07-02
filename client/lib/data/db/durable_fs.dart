import 'dart:ffi';
import 'dart:io';
import 'dart:isolate';

import 'package:ffi/ffi.dart';

/// File-durability primitives.
///
/// The migration atomic-swap and the atomic asset import depend on TRUE
/// durability: a referenced file's bytes AND the directory entry pointing at it
/// must be on permanent storage before the DB row that references it commits,
/// otherwise a power loss can leave a committed row pointing at a file that was
/// never actually flushed (a dangling reference / lost take).
///
/// On macOS a plain `fsync` does NOT flush the drive's write cache — only
/// `fcntl(fd, F_FULLFSYNC)` does — and a directory entry is only durable after
/// the directory fd itself is fsync'd. Neither is expressible in pure Dart, so
/// these call libc directly via FFI. On non-macOS targets (and if the native
/// call fails) they degrade to a best-effort flush.
class DurableFs {
  const DurableFs();

  /// Flush a file's data all the way to permanent storage (drive cache included
  /// on macOS via `F_FULLFSYNC`). Falls back to a buffered flush off-macOS.
  ///
  /// The native flush runs via [Isolate.run]: F_FULLFSYNC blocks until the
  /// drive's write cache is flushed (whole seconds for a large take on a slow
  /// disk), which would beachball the UI isolate if called inline. The closure
  /// is self-contained (path in, bool out) and the FFI globals below are
  /// lazily re-initialized inside the spawned isolate.
  Future<void> fsyncFile(String path) async {
    // O_WRONLY: F_FULLFSYNC wants a writable fd for a regular file.
    if (Platform.isMacOS &&
        await Isolate.run(
          () => _tryNativeFsync(path, _oWronly, fullFsync: true),
        )) {
      return;
    }
    // Fallback: push buffered writes to the OS (NOT a drive-cache flush).
    final raf = await File(path).open(mode: FileMode.append);
    try {
      await raf.flush();
    } finally {
      await raf.close();
    }
  }

  /// Flush a directory entry so a rename into it survives power loss. A directory
  /// fd can only be opened read-only, and its entries are made durable with a
  /// plain `fsync` on that fd. No pure-Dart fallback exists (best-effort no-op).
  Future<void> fsyncDir(String path) async {
    if (Platform.isMacOS) {
      // Off the UI isolate for the same reason as fsyncFile; awaited so the
      // caller's durability ordering (entry flushed before the DB commit) holds.
      await Isolate.run(
        () => _tryNativeFsync(path, _oRdonly, fullFsync: false),
      );
    }
  }

  /// Atomically replace [destPath] with [srcPath] (same filesystem). Rename is
  /// atomic on POSIX; the caller is responsible for both paths being on one
  /// volume (a cross-volume rename silently degrades to copy + unlink).
  Future<void> atomicRename(String srcPath, String destPath) async {
    await File(srcPath).rename(destPath);
  }
}

// --- libc FFI (macOS/POSIX) ---------------------------------------------------

// darwin fcntl.h constants.
const int _oRdonly = 0x0000;
const int _oWronly = 0x0001;
const int _fFullFsync = 51; // fcntl command: flush data + drive write cache.

final DynamicLibrary _libc = DynamicLibrary.process();

// open/fcntl are variadic; we bind fixed-arity forms that pass NO variadic arg
// (open without a mode, fcntl(F_FULLFSYNC) which ignores its third arg), which is
// ABI-safe on x86_64 and arm64 alike.
final int Function(Pointer<Utf8>, int) _open =
    _libc.lookupFunction<Int Function(Pointer<Utf8>, Int), int Function(Pointer<Utf8>, int)>('open');
final int Function(int, int) _fcntl =
    _libc.lookupFunction<Int Function(Int, Int), int Function(int, int)>('fcntl');
final int Function(int) _fsync =
    _libc.lookupFunction<Int Function(Int), int Function(int)>('fsync');
final int Function(int) _close =
    _libc.lookupFunction<Int Function(Int), int Function(int)>('close');

/// Opens [path] with [oflag], flushes it (F_FULLFSYNC when [fullFsync], else a
/// plain fsync), and closes it. Returns true only if the flush succeeded. Any
/// failure (unsupported platform, open error, exception) returns false so the
/// caller can fall back — durability is best-effort, never a crash.
bool _tryNativeFsync(String path, int oflag, {required bool fullFsync}) {
  Pointer<Utf8>? cPath;
  try {
    cPath = path.toNativeUtf8();
    final fd = _open(cPath, oflag);
    if (fd < 0) return false;
    try {
      if (fullFsync && _fcntl(fd, _fFullFsync) == 0) return true;
      // F_FULLFSYNC unsupported by the filesystem, or a directory fd: fall back
      // to fsync (still flushes the file/dir metadata through the OS).
      return _fsync(fd) == 0;
    } finally {
      _close(fd);
    }
  } catch (_) {
    return false;
  } finally {
    if (cPath != null) malloc.free(cPath);
  }
}
