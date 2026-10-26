import 'dart:io';

import 'package:path/path.dart' as p;

/// The files the exports in flight are writing.
final _writing = <String>{};

/// How much the exports in flight have written so far. A quit holding for an
/// export reads a change here as the export still working, not wedged (see
/// RecordingFinalizer): a long take copying to a slow stick outlasts the
/// quit's budget, and cutting it off left the file there cut short.
Future<int> exportBytesWritten() async {
  var n = 0;
  // Over a copy: an export ending (or starting) while a length is awaited
  // changes the set under the loop.
  for (final path in List.of(_writing)) {
    try {
      n += await File(path).length();
    } catch (_) {
      // Not created yet, or gone: nothing written there to count.
    }
  }
  return n;
}

/// Copies [source] to [destPath], as an export does: should the copy fail
/// part-way (a stick filling up), the half-written destination is removed
/// rather than left, truncated, where the user will find it. A destination
/// the copy never touched (it failed before writing) stays, even one picked
/// to be replaced. Rethrows the copy's error.
Future<void> exportCopy(File source, String destPath) async {
  // Onto the file a link names, not the link: the copy writes through it, and
  // a failed copy's cleanup (Dart's own, or the one below) removed the link
  // and left that file half-written.
  final dest = File(await exportTarget(destPath));
  ({DateTime modified, int size})? before;
  try {
    if (await dest.exists()) {
      before = (modified: await dest.lastModified(), size: await dest.length());
    }
  } on FileSystemException {
    // Unreadable: judged as absent, so only a file the copy then made goes.
  }
  _writing.add(dest.path);
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
  } finally {
    _writing.remove(dest.path);
  }
}

/// The file a copy onto [path] writes: [path], or the end of the links it
/// starts (one that names nothing yet included; at most 32 hops).
Future<String> exportTarget(String path) async {
  var current = path;
  try {
    for (var hop = 0; hop < 32; hop++) {
      if (!await FileSystemEntity.isLink(current)) break;
      final target = await Link(current).target();
      current = p.isAbsolute(target)
          ? target
          : p.join(p.dirname(current), target);
    }
  } on FileSystemException {
    // Unreadable: the copy itself will say so.
  }
  return current;
}

/// Whether the file a copy onto [path] writes lies inside [root], links in
/// both followed. False when either cannot be resolved (the copy then reports
/// its own error).
Future<bool> exportsInto(String root, String path) async {
  try {
    final dir = await Directory(root).resolveSymbolicLinks();
    final target = await exportTarget(path);
    final file = await File(target).exists()
        ? await File(target).resolveSymbolicLinks()
        : p.join(
            await Directory(p.dirname(target)).resolveSymbolicLinks(),
            p.basename(target),
          );
    return p.isWithin(dir, file);
  } on FileSystemException {
    return false;
  }
}
