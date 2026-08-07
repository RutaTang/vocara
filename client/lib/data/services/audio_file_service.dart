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

  /// Last line of defense for ids used as path components (the sync wire
  /// door already enforces this shape): a separator or `..` would escape
  /// the audio tree.
  static void _checkPathComponent(String practiceId) {
    if (!isPathComponentId(practiceId)) {
      throw ArgumentError.value(
        practiceId,
        'practiceId',
        'not a well-formed id',
      );
    }
  }

  Future<Directory> _audioDir(String practiceId) async {
    _checkPathComponent(practiceId);
    final base = await _storage.assetDir('audio');
    final dir = Directory(p.join(base.path, practiceId));
    if (!await dir.exists()) {
      await dir.create(recursive: true);
    }
    return dir;
  }

  /// Hard ceiling on any single imported file. Derivation: the longest media
  /// playback will ever decode is 60 minutes, and the fattest legal 60-minute
  /// source the decode budgets admit (96 kHz stereo float32 WAV) is ~2.8 GB;
  /// 4 GB clears that with headroom while refusing movie-sized junk BEFORE a
  /// full copy lands in the sandbox. Anything bigger could never play or
  /// analyze here anyway — copying it first was pure disk burn.
  static const int maxImportBytes = 4 * 1000 * 1000 * 1000;

  /// Copies [sourcePath] into the practice's audio directory and returns the
  /// new sandbox path. Throws [FormatException] when the source exceeds
  /// [maxImportBytes] — judged from a stat before any bytes move, and
  /// re-enforced during the copy itself (see [copyBounded]).
  Future<String> importAudio({
    required String practiceId,
    required String sourcePath,
  }) async {
    // Order: pure id validation, then the source stat, then any mkdir — a
    // refusal must precede every filesystem side effect.
    _checkPathComponent(practiceId);
    if (await File(sourcePath).length() > maxImportBytes) {
      throw const FormatException(
        'This file is too large to import. Media longer than an hour is not '
        'supported.',
      );
    }
    final dir = await _audioDir(practiceId);
    final ext = p.extension(sourcePath);
    final dest = p.join(dir.path, '${newId()}$ext');
    await copyBounded(sourcePath, dest);
    return dest;
  }

  /// Streams [sourcePath] to [destPath] with [maxBytes] enforced DURING the
  /// copy — the pre-copy stat is advisory only, since a source still being
  /// written (a download, an export in progress) can grow past it after the
  /// check. The bytes land in a uniquely named sibling temp first and reach
  /// [destPath] only by rename, so no failure mode leaves a half file at the
  /// final path: in `audio/<practiceId>/` a half-copied canonical WAV is
  /// exactly what boot recovery would resurrect as a phantom take.
  static Future<void> copyBounded(
    String sourcePath,
    String destPath, {
    int maxBytes = maxImportBytes,
  }) async {
    final tmp = File(
      '$destPath.import.$pid.${DateTime.now().microsecondsSinceEpoch}.tmp',
    );
    final sink = tmp.openWrite();
    var copied = 0;
    try {
      // addStream applies backpressure and forwards a mid-stream throw, so
      // the ceiling cuts the copy off promptly with a bounded footprint.
      await sink.addStream(
        File(sourcePath).openRead().map((chunk) {
          copied += chunk.length;
          if (copied > maxBytes) {
            throw const FormatException(
              'This file is too large to import. Media longer than an hour '
              'is not supported.',
            );
          }
          return chunk;
        }),
      );
      await sink.close();
      await tmp.rename(destPath);
    } catch (_) {
      try {
        await sink.close();
      } catch (_) {}
      try {
        if (await tmp.exists()) await tmp.delete();
      } catch (_) {}
      rethrow;
    }
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
