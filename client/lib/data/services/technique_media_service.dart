import 'dart:io';

import 'package:path/path.dart' as p;

import '../../utils/id.dart';
import '../db/asset_store.dart';
import 'audio_file_service.dart';
import 'local_storage_service.dart';

/// Owns technique example clips on disk. Imported files are copied into the app
/// sandbox (under `technique_media/<techniqueId>/`) so a technique owns its
/// media and never depends on the user's original file staying put. Mirrors
/// [AudioFileService].
class TechniqueMediaService {
  TechniqueMediaService({required LocalStorageService storage})
    : _storage = storage;

  final LocalStorageService _storage;

  /// Last line of defense for ids used as path components (the wire door
  /// already enforces this shape): a separator or `..` in an id would escape
  /// the media tree — worst on the recursive delete below.
  static void _checkPathComponent(String id) {
    if (!isPathComponentId(id)) {
      throw ArgumentError.value(id, 'techniqueId', 'not a well-formed id');
    }
  }

  Future<Directory> _mediaDir(String techniqueId) async {
    _checkPathComponent(techniqueId);
    final base = await _storage.assetDir('technique_media');
    final dir = Directory(p.join(base.path, techniqueId));
    if (!await dir.exists()) {
      await dir.create(recursive: true);
    }
    return dir;
  }

  /// Copies [sourcePath] into the technique's media directory and returns the
  /// new sandbox path (preserving the original extension). Throws
  /// [FormatException] past [AudioFileService.maxImportBytes] — judged from a
  /// stat, before any bytes move (a full copy of an oversized file was pure
  /// disk burn for media that could never decode here).
  Future<String> importMedia({
    required String techniqueId,
    required String sourcePath,
  }) async {
    // Order: pure id validation, then the source stat, then any mkdir — a
    // refusal must precede every filesystem side effect.
    _checkPathComponent(techniqueId);
    if (await File(sourcePath).length() > AudioFileService.maxImportBytes) {
      throw const FormatException(
        'This file is too large to import. Media longer than an hour is not '
        'supported.',
      );
    }
    final dir = await _mediaDir(techniqueId);
    final ext = p.extension(sourcePath);
    final dest = p.join(dir.path, '${newId()}$ext');
    // Bounded streaming copy via temp + rename: the stat above is advisory
    // (a still-growing source can pass it), and a half file must never sit
    // at the final path. See AudioFileService.copyBounded.
    await AudioFileService.copyBounded(sourcePath, dest);
    return dest;
  }

  /// Deletes a single copied media file. REFUSES content-addressed pool files
  /// (under `assets/`, shared by content hash across techniques and clips) and
  /// `asset://` references: those are owned by the [AssetStore] and reclaimed
  /// only by its hash-refcount GC. Persisted examples hydrate to pool paths,
  /// so deleting one by path would wipe the audio of every other row that
  /// dedups to the same bytes. Same guards as AudioFileService.deleteAudio.
  /// Silently ignores a missing file.
  Future<void> deleteMedia(String path) async {
    if (AssetStore.assetIdOf(path) != null) return;
    // Path-only guard: a delete must never create the pool directory as a
    // side effect (and must still resolve while the root is being torn down).
    final pool = await _storage.assetPath('assets');
    if (p.isWithin(pool, path)) return;
    try {
      await File(path).delete();
    } on PathNotFoundException {
      // Already gone (or a concurrent cleanup won): deletion achieved.
    }
  }

  /// Removes the whole media directory for a deleted technique (best effort).
  Future<void> deleteTechniqueMedia(String techniqueId) async {
    _checkPathComponent(techniqueId);
    final base = await _storage.assetPath('technique_media');
    try {
      await Directory(p.join(base, techniqueId)).delete(recursive: true);
    } on PathNotFoundException {
      // Never had media, or it is already gone: deletion achieved.
    }
  }
}
