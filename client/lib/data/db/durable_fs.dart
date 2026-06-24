import 'dart:io';

/// File-durability primitives. Today this is BEST-EFFORT (a Dart
/// [RandomAccessFile.flush] only pushes buffered writes to the OS, and there is
/// no way to fsync a directory from pure Dart).
///
/// The migration atomic-swap and the atomic asset import depend on TRUE
/// durability, so this is the seam where a native `fcntl(F_FULLFSYNC)` (plus a
/// directory fd fsync) will be wired in via a tiny platform channel. Until then,
/// power-loss durability is not guaranteed and the "structurally impossible
/// dangling reference" property is downgraded to "very unlikely".
class DurableFs {
  const DurableFs();

  /// Flush a file's data toward disk. Best-effort until the native
  /// F_FULLFSYNC primitive replaces it.
  Future<void> fsyncFile(String path) async {
    final raf = await File(path).open(mode: FileMode.append);
    try {
      await raf.flush();
    } finally {
      await raf.close();
    }
  }

  /// Flush a directory entry (so a rename into it is durable). Not expressible
  /// in pure Dart — a no-op placeholder for the native implementation.
  Future<void> fsyncDir(String path) async {}

  /// Atomically replace [destPath] with [srcPath] (same filesystem). Rename is
  /// atomic on POSIX; the caller is responsible for both paths being on one
  /// volume (a cross-volume rename silently degrades to copy + unlink).
  Future<void> atomicRename(String srcPath, String destPath) async {
    await File(srcPath).rename(destPath);
  }
}
