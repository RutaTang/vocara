import 'dart:io';

import 'package:path/path.dart' as p;

/// Copies [source] to [destPath], as an export does: should the copy fail
/// part-way (a stick filling up), the half-written destination is removed
/// rather than left, truncated, where the user will find it. A destination
/// the copy never touched (it failed before writing) stays, even one picked
/// to be replaced. Rethrows the copy's error.
Future<void> exportCopy(File source, String destPath) async {
  // Onto the file a link names, not the link: the copy writes through it, and
  // a failed copy's cleanup (Dart's own, or the one below) removed the link
  // and left that file half-written.
  final dest = File(await _throughLink(destPath));
  ({DateTime modified, int size})? before;
  try {
    if (await dest.exists()) {
      before = (modified: await dest.lastModified(), size: await dest.length());
    }
  } on FileSystemException {
    // Unreadable: judged as absent, so only a file the copy then made goes.
  }
  try {
    await source.copy(dest.path);
  } catch (_) {
    try {
      // Judged on the size too: a rewrite within the clock's tick (a second
      // on some disks) reads the old time.
      if (await dest.exists() &&
          (before == null ||
              await dest.lastModified() != before.modified ||
              await dest.length() != before.size)) {
        await dest.delete();
      }
    } on FileSystemException {
      // Best effort: the copy's own error is what the caller reports.
    }
    rethrow;
  }
}

/// [path], or the file it names when it is a link (one that names nothing
/// yet included).
Future<String> _throughLink(String path) async {
  try {
    if (!await FileSystemEntity.isLink(path)) return path;
    if (await File(path).exists()) {
      return await File(path).resolveSymbolicLinks();
    }
    final target = await Link(path).target();
    return p.isAbsolute(target) ? target : p.join(p.dirname(path), target);
  } on FileSystemException {
    return path;
  }
}
