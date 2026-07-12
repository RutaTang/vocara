import 'dart:io';

import 'package:path/path.dart' as p;

import '../../utils/id.dart';
import '../db/asset_store.dart';
import 'local_storage_service.dart';

/// Owns technique example clips on disk. Imported files are copied into the app
/// sandbox (under `technique_media/<techniqueId>/`) so a technique owns its
/// media and never depends on the user's original file staying put. Mirrors
/// [AudioFileService].
class TechniqueMediaService {
  TechniqueMediaService({required LocalStorageService storage})
    : _storage = storage;

  final LocalStorageService _storage;

  Future<Directory> _mediaDir(String techniqueId) async {
    final base = await _storage.assetDir('technique_media');
    final dir = Directory(p.join(base.path, techniqueId));
    if (!await dir.exists()) {
      await dir.create(recursive: true);
    }
    return dir;
  }

  /// Copies [sourcePath] into the technique's media directory and returns the
  /// new sandbox path (preserving the original extension).
  Future<String> importMedia({
    required String techniqueId,
    required String sourcePath,
  }) async {
    final dir = await _mediaDir(techniqueId);
    final ext = p.extension(sourcePath);
    final dest = p.join(dir.path, '${newId()}$ext');
    await File(sourcePath).copy(dest);
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
    final pool = await _storage.assetDir('assets');
    if (p.isWithin(pool.path, path)) return;
    final file = File(path);
    if (await file.exists()) {
      await file.delete();
    }
  }

  /// Removes the whole media directory for a deleted technique (best effort).
  Future<void> deleteTechniqueMedia(String techniqueId) async {
    final base = await _storage.assetDir('technique_media');
    final dir = Directory(p.join(base.path, techniqueId));
    if (await dir.exists()) {
      await dir.delete(recursive: true);
    }
  }
}
