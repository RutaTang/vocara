import 'dart:io';
import 'dart:typed_data';

import 'package:path/path.dart' as p;

import '../../utils/export_copy.dart';
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
/// (under `audio/<practiceId>/imports/`) so a practice owns its media and never
/// loses a reference to an external file. Recordings are written straight into
/// `audio/<practiceId>/`.
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

  Future<Directory> _audioDir(String practiceId, {String? subdir}) async {
    _checkPathComponent(practiceId);
    final base = await _storage.assetDir('audio');
    final dir = Directory(
      subdir == null
          ? p.join(base.path, practiceId)
          : p.join(base.path, practiceId, subdir),
    );
    if (!await dir.exists()) {
      await dir.create(recursive: true);
    }
    return dir;
  }

  /// Where imports stage: a subdirectory of the practice's staging tree, NOT
  /// the tree itself. Boot recovery (Persistence.recoverStagedRecordings)
  /// adopts every recorder-shaped WAV lying DIRECTLY under
  /// `audio/<practiceId>/`, and an import of a mono 48 kHz canonical WAV has
  /// exactly that shape (ensureStreamableWav keeps it as the track's file
  /// until the save pools it). A quit or crash before that save brought it
  /// back as a phantom "(recovered)" take. One level down it is invisible to
  /// recovery, which skips subdirectories, and the boot staging sweep still
  /// reclaims it with the rest of `audio/` (the same rule edit outputs follow
  /// under `edits/`, see AudioEditService).
  static const importsSubdir = 'imports';

  /// Hard ceiling on any single imported file. Derivation: the longest media
  /// playback will ever decode is 60 minutes, and the fattest legal 60-minute
  /// source the decode budgets admit (96 kHz stereo float32 WAV) is ~2.8 GB;
  /// 4 GB clears that with headroom while refusing movie-sized junk BEFORE a
  /// full copy lands in the sandbox. Anything bigger could never play or
  /// analyze here anyway — copying it first was pure disk burn.
  static const int maxImportBytes = 4 * 1000 * 1000 * 1000;

  /// Copies [sourcePath] into the practice's import staging directory (see
  /// [importsSubdir]) and returns the new sandbox path. Throws
  /// [FormatException] when the source exceeds [maxImportBytes] — judged from
  /// a stat before any bytes move, and re-enforced during the copy itself
  /// (see [copyBounded]).
  Future<String> importAudio({
    required String practiceId,
    required String sourcePath,
  }) async {
    // Order: pure id validation, then the source stat, then any mkdir — a
    // refusal must precede every filesystem side effect.
    _checkPathComponent(practiceId);
    // A plain file only: a pipe reports no length and held the copy open for
    // good, and a link to a device (/dev/zero) copied up to the cap.
    if (await FileSystemEntity.type(sourcePath) != FileSystemEntityType.file) {
      throw const FormatException("The file couldn't be read.");
    }
    final length = await File(sourcePath).length();
    if (length == 0) {
      throw const FormatException('This file is empty.');
    }
    if (length > maxImportBytes) {
      throw const FormatException(
        'This file is too large to import. Media longer than an hour is not '
        'supported.',
      );
    }
    final dir = await _audioDir(practiceId, subdir: importsSubdir);
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
  ///
  /// A take records into lane [takeId] from [startMs] on the timeline, and the
  /// name carries both (see [recordingPlacementOf]): after a crash, boot
  /// recovery found neither, so it added the take as a new lane at the
  /// timeline's start, and the empty lane REC had made for it stayed behind.
  Future<String> newRecordingPath({
    required String practiceId,
    String extension = 'wav',
    String? takeId,
    int? startMs,
  }) async {
    final dir = await _audioDir(practiceId);
    // Only an id of the minted shape goes into a file name.
    final placed =
        takeId != null &&
            isPathComponentId(takeId) &&
            startMs != null &&
            startMs >= 0
        ? '~$takeId~$startMs'
        : '';
    return p.join(dir.path, '${newId()}$placed.$extension');
  }

  /// The lane and timeline position a recording at [path] was made for, as
  /// [newRecordingPath] named it — or, once the take's backing started, as
  /// [writeAlignedStart] put it beside it; null for any other name (one from
  /// a build before it did, or not a recording). The start is negative when
  /// the take's audio begins before the timeline does: that much of the
  /// file's head is still to be cut (see [writeAlignedStart]).
  static Future<({String takeId, int startMs})?> recordingPlacementOf(
    String path,
  ) async {
    final parts = p.basenameWithoutExtension(path).split('~');
    if (parts.length != 3 || !isPathComponentId(parts[1])) return null;
    final startMs = int.tryParse(parts[2]);
    if (startMs == null || startMs < 0) return null;
    int? aligned;
    try {
      final f = File(_alignedStartOf(path));
      if (await f.exists()) aligned = int.tryParse(await f.readAsString());
    } catch (_) {
      // Unreadable: placed at REC instead, late by the arm and the latency.
    }
    return (takeId: parts[1], startMs: aligned ?? startMs);
  }

  /// Records beside the take at [path] where its audio starts on the
  /// timeline: once its backing has started, the playhead at REC less the
  /// file's head recorded before the backing rolled and the input latency —
  /// where a stop would place it; and from the stop on, where it did. The
  /// name holds only the playhead at REC, so a take recovered by it alone
  /// sat late by both.
  ///
  /// A negative start says the file's head is still to be cut. Whoever cuts
  /// it writes the start it leaves FIRST, and cuts only once that write is
  /// on disk: the other way round, a crash in between left a cut file still
  /// saying to cut, and the next recovery cut it again. Answers whether the
  /// write landed; best effort otherwise.
  Future<bool> writeAlignedStart(String path, int startMs) =>
      writeAlignedStartTo(path, startMs);

  /// [writeAlignedStart] for callers without a service (boot recovery).
  ///
  /// Writes to one path land in call order, each whole: the start's write is
  /// not awaited, and on a slow disk the stop's ran alongside it, both
  /// truncating one file. It was left `-512` (recovery cut the head the stop
  /// had already cut) or `0512` (placed half a second late).
  static Future<bool> writeAlignedStartTo(String path, int startMs) {
    final place = _alignedStartOf(path);
    final Future<bool> mine = (_placeWrites[place] ?? Future.value(true)).then(
      (_) => _writePlace(place, startMs),
    );
    _placeWrites[place] = mine;
    mine.whenComplete(() {
      if (identical(_placeWrites[place], mine)) _placeWrites.remove(place);
    });
    return mine;
  }

  /// The latest write queued per sidecar path ([writeAlignedStartTo]).
  static final Map<String, Future<bool>> _placeWrites = {};

  /// Writes [startMs] to [place] through a temp file and a rename, so a
  /// crash mid-write leaves the previous value rather than a cut-off one.
  static Future<bool> _writePlace(String place, int startMs) async {
    try {
      final tmp = File('$place.tmp');
      await tmp.writeAsString('$startMs', flush: true);
      await tmp.rename(place);
      return true;
    } catch (_) {
      // Recovery falls back to the playhead at REC.
      return false;
    }
  }

  /// The sidecar [writeAlignedStart] writes; the boot sweep keeps it with
  /// its recording while that is held back, and clears it with it.
  static String _alignedStartOf(String path) => '$path.place';

  /// Whether an export onto [path] would write inside the app's own storage
  /// (the folder holding the recordings, the take pool and the library
  /// database), links followed: it overwrote another take's audio, or the
  /// library itself.
  Future<bool> isAppStorage(String path) async {
    final String root;
    try {
      root = p.dirname(await _storage.assetPath('audio'));
    } catch (_) {
      return false; // no storage to resolve (the copy then says what failed)
    }
    return exportsInto(root, path);
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
  /// writes). Reads only the header, never the payload, so it is cheap even on
  /// an hours-long take.
  ///
  /// Null answers ONE question — "is this the recorder's shape?" — and means
  /// too short, not that layout, not plain PCM, or gone. An unreadable file
  /// THROWS instead, and that distinction is load-bearing: the boot-time orphan
  /// recovery treats null as "provably not a recording, leave it for the
  /// sweep", which trashes the file. A hardware or filesystem fault must not
  /// be able to answer that question, or the only copy of a crashed take is
  /// swept away on the strength of a read that never happened. A thrown probe
  /// lands in the recovery's retry hold-back instead, and the take survives to
  /// the next boot.
  static Future<WavProbe?> probeWav(File file) async {
    const headerLen = 44;
    final int length;
    try {
      length = await file.length();
    } on PathNotFoundException {
      // Vanished between the directory listing and here: nothing to recover,
      // and nothing to hold back for. Every other IO failure propagates.
      return null;
    }
    if (length < headerLen) return null;

    final Uint8List head;
    final RandomAccessFile raf;
    try {
      raf = await file.open();
    } on PathNotFoundException {
      return null;
    }
    try {
      head = await raf.read(headerLen);
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
