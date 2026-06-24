import 'dart:io';

import 'package:path/path.dart' as p;

import '../../utils/id.dart';
import 'local_storage_service.dart';

/// Owns audio files on disk. Imported files are copied into the app sandbox
/// (under `audio/<practiceId>/`) so a practice owns its media and never loses
/// a reference to an external file. Recordings are written straight here.
class AudioFileService {
  AudioFileService({required LocalStorageService storage}) : _storage = storage;

  final LocalStorageService _storage;

  Future<Directory> _audioDir(String practiceId) async {
    final base = await _storage.assetDir('audio');
    final dir = Directory(p.join(base.path, practiceId));
    if (!await dir.exists()) {
      await dir.create(recursive: true);
    }
    return dir;
  }

  /// Copies [sourcePath] into the practice's audio directory and returns the
  /// new sandbox path.
  Future<String> importAudio({
    required String practiceId,
    required String sourcePath,
  }) async {
    final dir = await _audioDir(practiceId);
    final ext = p.extension(sourcePath);
    final dest = p.join(dir.path, '${newId()}$ext');
    await File(sourcePath).copy(dest);
    return dest;
  }

  /// Returns a fresh path inside the sandbox for a new recording. The recorder
  /// writes directly to this path.
  Future<String> newRecordingPath({
    required String practiceId,
    String extension = 'wav',
  }) async {
    final dir = await _audioDir(practiceId);
    return p.join(dir.path, '${newId()}.$extension');
  }

  /// Deletes a per-practice working audio file. REFUSES content-addressed pool
  /// files (under `assets/`, shared by content hash across clips) and
  /// `asset://` references: those are owned by the AssetStore and reclaimed by
  /// its hash-refcount GC. Deleting a pooled file by path would wipe the audio
  /// of every other clip that dedups to the same hash.
  Future<void> deleteAudio(String path) async {
    if (path.startsWith('asset://')) return;
    final pool = await _storage.assetDir('assets');
    if (p.isWithin(pool.path, path)) return;
    final file = File(path);
    if (await file.exists()) {
      await file.delete();
    }
  }
}
