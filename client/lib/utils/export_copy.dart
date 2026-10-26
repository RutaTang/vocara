import 'dart:io';

/// Copies [source] to [destPath], as an export does: should the copy fail
/// part-way (a stick filling up), the half-written destination is removed
/// rather than left, truncated, where the user will find it. A destination
/// the copy never touched (it failed before writing) stays, even one picked
/// to be replaced. Rethrows the copy's error.
Future<void> exportCopy(File source, String destPath) async {
  final dest = File(destPath);
  DateTime? before;
  try {
    if (await dest.exists()) before = await dest.lastModified();
  } on FileSystemException {
    // Unreadable: judged as absent, so only a file the copy then made goes.
  }
  try {
    await source.copy(destPath);
  } catch (_) {
    try {
      if (await dest.exists() &&
          (before == null || await dest.lastModified() != before)) {
        await dest.delete();
      }
    } on FileSystemException {
      // Best effort: the copy's own error is what the caller reports.
    }
    rethrow;
  }
}
