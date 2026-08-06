import 'dart:io';
import 'dart:typed_data';

import 'package:path/path.dart' as p;

import '../../utils/id.dart';
import 'local_storage_service.dart';

/// Header facts of a canonical PCM WAV file — see [AudioFileService.probeWav].
/// [dataBytes] is the payload the header AND the file both cover: the header's
/// data-chunk size clamped to the bytes actually present after the header (a
/// hard-killed recording legitimately holds a written-but-not-yet-patched tail
/// beyond the last header patch; that tail is not counted).
typedef WavProbe = ({
  int channels,
  int sampleRate,
  int bitsPerSample,
  int dataBytes,
});

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

  /// Parses the header of [file] as a canonical uncompressed-PCM WAV (`RIFF` /
  /// `WAVE` / 16-byte `fmt ` / `data`, the exact layout the recording engine
  /// writes). Returns null when the file is too short, not that layout, or not
  /// plain PCM — the boot-time orphan recovery uses null as "not a recording,
  /// leave it for the sweep". Reads only the header, never the payload, so it
  /// is cheap even on an hours-long take.
  static Future<WavProbe?> probeWav(File file) async {
    const headerLen = 44;
    final int length;
    try {
      length = await file.length();
    } catch (_) {
      return null;
    }
    if (length < headerLen) return null;

    final Uint8List head;
    final raf = await file.open();
    try {
      head = await raf.read(headerLen);
    } catch (_) {
      return null;
    } finally {
      await raf.close();
    }
    if (head.length < headerLen) return null;

    final bd = ByteData.sublistView(head);
    String tag(int o) => String.fromCharCodes(head, o, o + 4);
    if (tag(0) != 'RIFF' || tag(8) != 'WAVE' || tag(12) != 'fmt ') return null;
    // Canonical layout only: a 16-byte fmt chunk immediately followed by the
    // data chunk (what WavStream writes). Foreign files with extensible fmt /
    // extra chunks are simply "not a recording" here. The engine records
    // 32-bit float (format 3); pre-float recordings were 16-bit PCM (format 1).
    if (bd.getUint32(16, Endian.little) != 16) return null;
    final format = bd.getUint16(20, Endian.little);
    final channels = bd.getUint16(22, Endian.little);
    final sampleRate = bd.getUint32(24, Endian.little);
    final bits = bd.getUint16(34, Endian.little);
    final isPcm16 = format == 1 && bits == 16;
    final isFloat32 = format == 3 && bits == 32;
    if (!isPcm16 && !isFloat32) return null;
    if (tag(36) != 'data') return null;
    if (channels < 1 || sampleRate <= 0) return null;
    final headerDataBytes = bd.getUint32(40, Endian.little);
    // The payload both the header and the file cover. min() tolerates the
    // hard-kill shape (real bytes past the last header patch) and rejects a
    // header that claims more than the file holds.
    final dataBytes = headerDataBytes < (length - headerLen)
        ? headerDataBytes
        : length - headerLen;
    return (
      channels: channels,
      sampleRate: sampleRate,
      bitsPerSample: bits,
      dataBytes: dataBytes,
    );
  }
}
