import 'dart:ffi' as ffi;
import 'dart:io';
import 'dart:isolate';
import 'dart:math' as math;
import 'dart:typed_data';

import 'package:ffi/ffi.dart';
import 'package:flutter/foundation.dart' show visibleForTesting;
import 'package:path/path.dart' as p;

import '../db/durable_fs.dart';
import '../ffi/vocara_engine_bindings.dart';
import 'audio_file_service.dart';
import 'graph_audio_playback_service.dart'
    show decodeStatusToException, maxPlaybackDuration;

/// Open the embedded engine framework. Top-level so a background isolate can
/// load it without capturing `this`.
ffi.DynamicLibrary _openVocaraLibrary() {
  const names = [
    'vocara_engine.framework/vocara_engine',
    'vocara_engine.framework/Versions/A/vocara_engine',
  ];
  for (final n in names) {
    try {
      return ffi.DynamicLibrary.open(n);
    } catch (_) {
      // try next
    }
  }
  return ffi.DynamicLibrary.process();
}

/// Parsed layout of a WAV file: the original `fmt ` chunk body (preserved so
/// the exact sample format is kept on re-emit) plus where the PCM data lives.
class _WavLayout {
  _WavLayout(
    this.fmt,
    this.sampleRate,
    this.blockAlign,
    this.dataStart,
    this.dataLen,
  );

  final Uint8List fmt;
  final int sampleRate;
  final int blockAlign; // bytes per frame (channels * bits/8)
  final int dataStart;
  final int dataLen;
}

/// Little-endian u16 at [off] within a `fmt ` chunk body (-1 if out of range).
int _fmtU16(Uint8List f, int off) =>
    off + 1 < f.length ? f[off] | (f[off + 1] << 8) : -1;

/// The effective sample TYPE: for WAVE_FORMAT_EXTENSIBLE (0xFFFE) it's the
/// SubFormat GUID's leading format code at fmt offset 24; otherwise the format
/// tag at offset 0 (1 = integer PCM, 3 = IEEE float).
int _effFmt(Uint8List f) {
  final tag = _fmtU16(f, 0);
  return tag == 0xFFFE ? _fmtU16(f, 24) : tag;
}

/// Scans a WAV's chunks generically (tolerating extra chunks and
/// WAVE_FORMAT_EXTENSIBLE). Returns null if it isn't an uncompressed WAV.
_WavLayout? _parseWav(Uint8List bytes) {
  if (bytes.length < 12) return null;
  String tag(int o) => String.fromCharCodes(bytes, o, o + 4);
  if (tag(0) != 'RIFF' || tag(8) != 'WAVE') return null;

  final bd = ByteData.sublistView(bytes);
  Uint8List? fmt;
  var sampleRate = 0, blockAlign = 0, dataStart = -1, dataLen = 0;
  var offset = 12;
  while (offset + 8 <= bytes.length) {
    final id = tag(offset);
    final size = bd.getUint32(offset + 4, Endian.little);
    final body = offset + 8;
    if (id == 'fmt ' && body + 16 <= bytes.length) {
      final end = math.min(body + size, bytes.length);
      fmt = Uint8List.fromList(bytes.sublist(body, end));
      sampleRate = bd.getUint32(body + 4, Endian.little);
      blockAlign = bd.getUint16(body + 12, Endian.little);
    } else if (id == 'data') {
      dataStart = body;
      dataLen = math.min(size, bytes.length - body);
      break;
    }
    offset = body + size + (size & 1); // chunks are word-aligned
  }
  if (fmt == null || dataStart < 0 || blockAlign <= 0 || sampleRate <= 0) {
    return null;
  }
  // Only uncompressed samples can be byte-spliced with PCM frame math. A
  // compressed RIFF payload (ADPCM / mp3-in-WAV / mu-law) parses the same
  // chunks, but its blockAlign is a codec block, not a PCM frame — splitting
  // it mid-block would produce silent nonsense. Returning null routes such
  // files to the callers' "compressed audio" FormatException.
  final eff = _effFmt(fmt);
  if (eff != 1 && eff != 3) return null;
  // Sanity-check the frame layout: an uncompressed blockAlign is exactly
  // channels * ceil(bits / 8); anything else is a lying header whose frame
  // math would shear every frame after the first cut.
  final channels = _fmtU16(fmt, 2);
  final bits = _fmtU16(fmt, 14);
  if (channels <= 0 ||
      bits <= 0 ||
      blockAlign != channels * ((bits + 7) ~/ 8)) {
    return null;
  }
  return _WavLayout(fmt, sampleRate, blockAlign, dataStart, dataLen);
}

/// Testable face of [_compatibleForMerge]: parses two whole-file byte
/// buffers and answers whether they can be BYTE-spliced under the first's
/// header (false routes concat through the decode → re-encode path); null
/// when either buffer is not an uncompressed WAV.
@visibleForTesting
bool? wavCompatibleForMerge(Uint8List a, Uint8List b) {
  final wa = _parseWav(a);
  final wb = _parseWav(b);
  if (wa == null || wb == null) return null;
  return _compatibleForMerge(wa, wb);
}

/// Whether two WAVs can be concatenated losslessly under ONE fmt header:
/// same sample rate, channel count, bit depth, effective sample type, and
/// (for EXTENSIBLE) valid bits + channel mask. The format TAG may differ
/// (plain PCM vs EXTENSIBLE of the same actual format).
bool _compatibleForMerge(_WavLayout a, _WavLayout b) {
  // Comparing the effective format — not just rate/channels/bits — stops a
  // 32-bit int WAV and a 32-bit float WAV (same rate/channels/depth) from
  // merging into reinterpreted noise, while still allowing plain PCM vs
  // EXTENSIBLE-PCM of the same actual format (both resolve to 1).
  if (a.sampleRate != b.sampleRate ||
      _fmtU16(a.fmt, 2) != _fmtU16(b.fmt, 2) ||
      _fmtU16(a.fmt, 14) != _fmtU16(b.fmt, 14) ||
      _effFmt(a.fmt) != _effFmt(b.fmt)) {
    return false;
  }
  // EXTENSIBLE extras: the merged file keeps A's header, so B's samples
  // would be reinterpreted under A's valid-bits (24-in-32 vs full 32 is
  // real corruption, not metadata) and rendered under A's channel mask.
  // A plain-fmt file implies validBits == bits and no mask claim.
  int validBits(_WavLayout w) =>
      w.fmt.length >= 40 ? _fmtU16(w.fmt, 18) : _fmtU16(w.fmt, 14);
  int mask(_WavLayout w) =>
      w.fmt.length >= 40 ? _fmtU16(w.fmt, 20) | (_fmtU16(w.fmt, 22) << 16) : 0;
  if (validBits(a) != validBits(b)) return false;
  // The mask is compared unconditionally, in both directions. `mask()` already
  // answers 0 for a plain fmt, which is exactly the spec's "speaker positions
  // unspecified" — so a plain stereo and an EXTENSIBLE stereo carrying, say,
  // Left+Centre are genuinely different claims about the same bytes, and the
  // splice would stamp A's claim over B's half. Only comparing when BOTH sides
  // were EXTENSIBLE let that through either way round.
  if (mask(a) != mask(b)) return false;
  return true;
}

/// Cuts the first [trimMs] of audio out of the uncompressed WAV at [path],
/// IN PLACE (temp file + atomic rename, so a crash mid-write leaves either the
/// old file or the new one — never a torn WAV). Frame-quantized; preserves the
/// exact sample format. Used by take placement when the computed start would
/// land before timeline zero: the pre-roll head that precedes t=0 must be cut
/// out of the FILE, otherwise clamping the position silently discards the
/// entire pre-roll/latency compensation. Throws [FormatException] when [path]
/// is not an uncompressed WAV (callers treat that as best-effort and fall back
/// to the un-trimmed take).
///
/// Runs in a background isolate and STREAMS the copy in bounded chunks: a long
/// take is hundreds of megabytes, and the old whole-file
/// read → rebuild → write held two full copies on the MAIN isolate.
Future<void> trimWavHeadInPlace(String path, int trimMs) async {
  if (trimMs <= 0) return;
  final tmp = await Isolate.run(() => _trimWavHeadSync(path, trimMs));
  // The trimmed copy takes the name of the take's only copy (the save pools
  // it seconds later): its bytes go to disk before the rename gives it that
  // name, and the rename before the take is relied on. Unflushed, a power
  // cut in between could leave an empty file there, which boot recovery
  // takes for junk.
  const fs = DurableFs();
  try {
    await fs.fsyncFile(tmp);
    await File(tmp).rename(path);
  } catch (_) {
    // Neither flushed nor in place: the take keeps its head, and the copy
    // doesn't linger beside it.
    try {
      await File(tmp).delete();
    } catch (_) {}
    rethrow;
  }
  await fs.fsyncDir(p.dirname(path));
}

/// Isolate body of [trimWavHeadInPlace]: RandomAccessFile chunk walk (header
/// bytes only), then a chunked data copy into a temp file beside [path],
/// whose path it returns for the caller to make durable and rename.
String _trimWavHeadSync(String path, int trimMs) {
  const noWav = FormatException('Only uncompressed WAV takes can be trimmed.');
  final raf = File(path).openSync();
  Uint8List? fmt;
  var dataStart = -1, dataLen = 0;
  try {
    final fileLen = raf.lengthSync();
    final head = raf.readSync(12);
    String tag(Uint8List b, int o) => String.fromCharCodes(b, o, o + 4);
    if (head.length < 12 || tag(head, 0) != 'RIFF' || tag(head, 8) != 'WAVE') {
      throw noWav;
    }
    var offset = 12;
    // Bounded walk, mirroring _parseWav's chunk tolerance (see the analysis
    // gate on why 64 covers every real-world metadata pile-up).
    for (var i = 0; i < 64 && dataStart < 0; i++) {
      raf.setPositionSync(offset);
      final ch = raf.readSync(8);
      if (ch.length < 8) break;
      final id = tag(ch, 0);
      final size = ByteData.sublistView(ch).getUint32(4, Endian.little);
      if (id == 'fmt ' && size >= 16 && size <= 1024) {
        fmt = raf.readSync(size);
      } else if (id == 'data') {
        dataStart = offset + 8;
        dataLen = math.min(size, fileLen - dataStart);
      }
      offset += 8 + size + (size & 1); // chunks are word-aligned
    }
    final f = fmt;
    if (f == null || f.length < 16 || dataStart < 0) throw noWav;
    final bd = ByteData.sublistView(f);
    final sampleRate = bd.getUint32(4, Endian.little);
    final blockAlign = bd.getUint16(12, Endian.little);
    // Same admission rules as _parseWav: uncompressed samples only, and a
    // blockAlign that is a true PCM frame (see there for the rationale).
    final eff = _effFmt(f);
    final channels = _fmtU16(f, 2);
    final bits = _fmtU16(f, 14);
    if ((eff != 1 && eff != 3) ||
        sampleRate <= 0 ||
        channels <= 0 ||
        bits <= 0 ||
        blockAlign != channels * ((bits + 7) ~/ 8)) {
      throw noWav;
    }
    // Whole frames only (see concatWav on why alignment matters), and never
    // cut past the end — a trim longer than the take leaves an empty WAV.
    final wholeLen = (dataLen ~/ blockAlign) * blockAlign;
    final cut = ((trimMs / 1000 * sampleRate).round() * blockAlign).clamp(
      0,
      wholeLen,
    );
    final newLen = wholeLen - cut;

    // Unique temp name (pid + wall clock): the old fixed '$path.trim' was
    // predictable, so a pre-placed entry at that exact name could redirect or
    // tear a concurrent/re-entrant trim. Failed attempts are deleted below.
    final tmpPath =
        '$path.trim.$pid.${DateTime.now().microsecondsSinceEpoch}.tmp';
    final tmp = File(tmpPath);
    final out = tmp.openSync(mode: FileMode.write);
    var ok = false;
    try {
      final header = BytesBuilder();
      void str(String s) => header.add(s.codeUnits);
      void u32(int v) => header.add(
        (ByteData(4)..setUint32(0, v, Endian.little)).buffer.asUint8List(),
      );
      // Word alignment for an odd-length fmt body, as in _writeWavHeaderSync.
      final fmtPad = f.length.isOdd ? 1 : 0;
      final pad = newLen.isOdd ? 1 : 0;
      str('RIFF');
      u32(4 + (8 + f.length + fmtPad) + (8 + newLen + pad));
      str('WAVE');
      str('fmt ');
      u32(f.length);
      header.add(f);
      if (fmtPad == 1) header.addByte(0);
      str('data');
      u32(newLen);
      out.writeFromSync(header.toBytes());
      // Chunked copy: 4 MiB at a time keeps the peak bounded regardless of
      // the take's length.
      raf.setPositionSync(dataStart + cut);
      var remaining = newLen;
      while (remaining > 0) {
        final chunk = raf.readSync(math.min(remaining, 4 << 20));
        if (chunk.isEmpty) break; // truncated on disk: keep what exists
        out.writeFromSync(chunk);
        remaining -= chunk.length;
      }
      if (remaining == 0) {
        if (pad == 1) out.writeByteSync(0);
      } else {
        // The source was truncated under us: keep whole frames and patch
        // both size fields so the header never claims bytes the file does
        // not hold (a header written up front describes the EXPECTED length).
        _patchWavSizesSync(
          out,
          f,
          ((newLen - remaining) ~/ blockAlign) * blockAlign,
        );
      }
      ok = true;
    } finally {
      out.closeSync();
      // A failed attempt must not linger: the .tmp suffix is outside every
      // recovery scan, but disk space and confusion are still real.
      if (!ok) {
        try {
          tmp.deleteSync();
        } catch (_) {}
      }
    }
    return tmpPath;
  } finally {
    raf.closeSync();
  }
}

/// Streamed-parse of a WAV's header from an open [raf]: the same admission
/// rules as [_parseWav], without loading the file (whole-file reads made a
/// long take cost gigabytes across split/concat buffers). [dataLen] is
/// clamped to what the file physically holds, like [_parseWav].
_WavLayout? _parseWavFromRaf(RandomAccessFile raf) {
  final fileLen = raf.lengthSync();
  raf.setPositionSync(0);
  final head = raf.readSync(12);
  String tag(Uint8List b, int o) => String.fromCharCodes(b, o, o + 4);
  if (head.length < 12 || tag(head, 0) != 'RIFF' || tag(head, 8) != 'WAVE') {
    return null;
  }
  Uint8List? fmt;
  var dataStart = -1, dataLen = 0;
  var offset = 12;
  // Bounded walk, mirroring _parseWav's chunk tolerance.
  for (var i = 0; i < 64 && dataStart < 0; i++) {
    raf.setPositionSync(offset);
    final ch = raf.readSync(8);
    if (ch.length < 8) break;
    final id = tag(ch, 0);
    final size = ByteData.sublistView(ch).getUint32(4, Endian.little);
    if (id == 'fmt ' && size >= 16 && size <= 1024) {
      fmt = raf.readSync(size);
    } else if (id == 'data') {
      dataStart = offset + 8;
      dataLen = math.min(size, fileLen - dataStart);
    }
    offset += 8 + size + (size & 1); // chunks are word-aligned
  }
  final f = fmt;
  if (f == null || f.length < 16 || dataStart < 0 || dataLen < 0) return null;
  final bd = ByteData.sublistView(f);
  final sampleRate = bd.getUint32(4, Endian.little);
  final blockAlign = bd.getUint16(12, Endian.little);
  final eff = _effFmt(f);
  final channels = _fmtU16(f, 2);
  final bits = _fmtU16(f, 14);
  if ((eff != 1 && eff != 3) ||
      sampleRate <= 0 ||
      channels <= 0 ||
      bits <= 0 ||
      blockAlign != channels * ((bits + 7) ~/ 8)) {
    return null;
  }
  return _WavLayout(f, sampleRate, blockAlign, dataStart, dataLen);
}

/// Why [path] can't be edited when it doesn't parse as a WAV: [notWav], or,
/// when there is no file at all (its bytes haven't reached this device, or the
/// file was removed), that — not a format it doesn't have.
FormatException _cannotEdit(String path, String notWav) =>
    File(path).existsSync()
    ? FormatException(notWav)
    : const FormatException(
        "This take's audio file isn't on this device, so it can't be edited.",
      );

/// [_parseWavFromRaf] on a path: opens, parses the header, closes. Cheap
/// (a handful of small reads), so callers may run it on any isolate.
_WavLayout? _parseWavAtPath(String path) {
  final RandomAccessFile raf;
  try {
    raf = File(path).openSync();
  } on FileSystemException {
    return null;
  }
  try {
    return _parseWavFromRaf(raf);
  } finally {
    raf.closeSync();
  }
}

/// Patches the RIFF + data sizes written by [_writeWavHeaderSync] down to
/// [actualLen] (whole-frame bytes, caller-aligned) and truncates the torn
/// tail. Used when a source turned out shorter than its header promised: the
/// up-front header describes the EXPECTED length, and publishing it unpatched
/// means a header claiming bytes the file does not hold.
void _patchWavSizesSync(RandomAccessFile out, Uint8List fmt, int actualLen) {
  // Data starts after RIFF(12) + fmt header(8) + fmt body + its word-alignment
  // pad — the exact layout _writeWavHeaderSync emits.
  final fmtPad = fmt.length.isOdd ? 1 : 0;
  final dataStart = 28 + fmt.length + fmtPad;
  final pad = actualLen.isOdd ? 1 : 0;
  out.truncateSync(dataStart + actualLen);
  if (pad == 1) {
    out.setPositionSync(dataStart + actualLen);
    out.writeByteSync(0);
  }
  void u32At(int off, int v) {
    out.setPositionSync(off);
    out.writeFromSync(
      (ByteData(4)..setUint32(0, v, Endian.little)).buffer.asUint8List(),
    );
  }

  u32At(4, 4 + (8 + fmt.length + fmtPad) + (8 + actualLen + pad));
  u32At(dataStart - 4, actualLen);
}

/// Writes a WAV header (RIFF + preserved fmt + data size) to [out].
///
/// RIFF chunks are word-aligned: an odd-length fmt body (the parsers admit any
/// size in [16, 1024]) must be followed by a pad byte, or every chunk after it
/// sits at an odd offset and the aligned walkers — this file's own parsers and
/// symphonia's — read garbage where the next chunk id should be.
void _writeWavHeaderSync(RandomAccessFile out, Uint8List fmt, int dataLen) {
  final b = BytesBuilder();
  void str(String s) => b.add(s.codeUnits);
  void u32(int v) =>
      b.add((ByteData(4)..setUint32(0, v, Endian.little)).buffer.asUint8List());
  final fmtPad = fmt.length.isOdd ? 1 : 0;
  final pad = dataLen.isOdd ? 1 : 0;
  str('RIFF');
  u32(4 + (8 + fmt.length + fmtPad) + (8 + dataLen + pad));
  str('WAVE');
  str('fmt ');
  u32(fmt.length);
  b.add(fmt);
  if (fmtPad == 1) b.addByte(0);
  str('data');
  u32(dataLen);
  out.writeFromSync(b.toBytes());
}

/// Chunked byte-range copy (4 MiB at a time): the peak stays bounded no
/// matter how long the take is. Returns the bytes actually copied (a
/// truncated-on-disk source yields fewer; callers keep what exists, the same
/// tolerance the whole-file path had).
int _copyRangeSync(
  RandomAccessFile src,
  int start,
  int len,
  RandomAccessFile out,
) {
  src.setPositionSync(start);
  var remaining = len;
  var copied = 0;
  while (remaining > 0) {
    final chunk = src.readSync(math.min(remaining, 4 << 20));
    if (chunk.isEmpty) break;
    out.writeFromSync(chunk);
    copied += chunk.length;
    remaining -= chunk.length;
  }
  return copied;
}

/// Non-destructive clip edits on WAV files. Split/merge slice and join the raw
/// PCM **bytes** (preserving the source's exact sample format — 16/24/32-bit,
/// integer or float, plain or WAVE_FORMAT_EXTENSIBLE), so any uncompressed WAV
/// works losslessly. Recordings are WAV, so takes can always be edited.
class AudioEditService {
  AudioEditService({required AudioFileService files}) : _files = files;

  final AudioFileService _files;

  /// A fresh output path under `audio/<practiceId>/edits/` — a sibling of the
  /// take staging tree, NOT the tree itself. Boot recovery
  /// (Persistence.recoverStagedRecordings) adopts any orphan recording-shaped
  /// WAV lying DIRECTLY under `audio/<practiceId>/`, so writing edit outputs
  /// there means a kill between the file write and the save commit resurrects
  /// each output as a phantom "(recovered)" take next boot while the original
  /// clip is intact. Files under `edits/` are invisible to recovery (it skips
  /// subdirectories) but still reclaimed by the boot staging sweep, which
  /// trashes the whole `audio/` tree. The paths stay ordinary sandbox paths,
  /// so the save flow imports them into the pool exactly as before.
  Future<String> _newEditPath({
    required String practiceId,
    String extension = 'wav',
  }) async {
    final base = await _files.newRecordingPath(
      practiceId: practiceId,
      extension: extension,
    );
    final dir = Directory(p.join(p.dirname(base), 'edits'));
    await dir.create(recursive: true);
    return p.join(dir.path, p.basename(base));
  }

  /// Splits the WAV at [splitFileMs] (offset from the file's start) into two
  /// new WAV files. Returns their paths. Throws if the source isn't a WAV.
  Future<({String firstPath, String secondPath})> splitWav({
    required String practiceId,
    required String sourcePath,
    required int splitFileMs,
  }) async {
    // Header-only parse: the whole-file read held a full copy of a long take
    // (plus both slices and both outputs — gigabytes for a legal file); the
    // split is a byte-range operation and streams as one.
    final w = _parseWavAtPath(sourcePath);
    if (w == null) {
      throw _cannotEdit(
        sourcePath,
        'This take is compressed audio (e.g. mp3/m4a). Only recordings and '
        'WAV files can be split.',
      );
    }
    // Work in WHOLE frames: a data chunk that isn't a frame multiple (a header
    // from a write cut short) would otherwise let the clamp below land the cut
    // INSIDE a frame, and the second half would then start on the wrong channel
    // — swapped stereo for the rest of the take.
    final usable = (w.dataLen ~/ w.blockAlign) * w.blockAlign;
    // Need at least one frame on each side. A 0-length / sub-2-frame take (e.g. a
    // recording quit mid-capture) would otherwise give clamp bounds where lower >
    // upper, and num.clamp throws an opaque ArgumentError instead of this message.
    if (usable < 2 * w.blockAlign) {
      throw const FormatException('This take is too short to split.');
    }
    final frame = (splitFileMs / 1000 * w.sampleRate).round();
    final cut = (frame * w.blockAlign).clamp(
      w.blockAlign,
      usable - w.blockAlign,
    );

    final firstPath = await _newEditPath(practiceId: practiceId);
    final secondPath = await _newEditPath(practiceId: practiceId);
    await Isolate.run(() {
      final src = File(sourcePath).openSync();
      try {
        // Both halves are written to unique temps and published together at
        // the end: a failure mid-way must not leave one half sitting at a
        // final path masquerading as a finished edit.
        final nonce = '$pid.${DateTime.now().microsecondsSinceEpoch}';
        final tmpFirst = '$firstPath.$nonce.tmp';
        final tmpSecond = '$secondPath.$nonce.tmp';
        var ok = false;
        try {
          for (final (tmpPath, start, len) in [
            (tmpFirst, w.dataStart, cut),
            // `usable`, not dataLen: a trailing partial frame is dropped, not
            // carried into the second half as a half-frame of noise.
            (tmpSecond, w.dataStart + cut, usable - cut),
          ]) {
            final out = File(tmpPath).openSync(mode: FileMode.write);
            try {
              _writeWavHeaderSync(out, w.fmt, len);
              final copied = _copyRangeSync(src, start, len, out);
              if (copied == len) {
                if (len.isOdd) out.writeByteSync(0);
              } else {
                // Truncated on disk: keep what exists, header kept honest.
                _patchWavSizesSync(
                  out,
                  w.fmt,
                  (copied ~/ w.blockAlign) * w.blockAlign,
                );
              }
            } finally {
              out.closeSync();
            }
          }
          File(tmpFirst).renameSync(firstPath);
          File(tmpSecond).renameSync(secondPath);
          ok = true;
        } finally {
          if (!ok) {
            for (final t in [tmpFirst, tmpSecond]) {
              try {
                File(t).deleteSync();
              } catch (_) {}
            }
          }
        }
      } finally {
        src.closeSync();
      }
    });
    return (firstPath: firstPath, secondPath: secondPath);
  }

  /// Concatenates two WAVs end-to-end ([firstPath] then [secondPath]) into one
  /// new WAV (inverse of [splitWav]). Takes of one format are joined byte for
  /// byte; any other pair is re-encoded by the engine ([punchIn]).
  Future<String> concatWav({
    required String practiceId,
    required String firstPath,
    required String secondPath,
  }) async {
    // Header-only parses; the join itself streams below (the whole-file
    // reads held both takes, both slices, and the merged buffer at once).
    final wa = _parseWavAtPath(firstPath);
    final wb = _parseWavAtPath(secondPath);
    const compressed =
        'These takes are compressed audio. Only recordings and WAV files '
        'can be merged.';
    if (wa == null) throw _cannotEdit(firstPath, compressed);
    if (wb == null) throw _cannotEdit(secondPath, compressed);
    // The output reuses wa's fmt header for BOTH halves, so the takes must share
    // the fields that determine how the PCM bytes are read — sample rate,
    // channels, bit depth — otherwise the second take is reinterpreted under the
    // first's format (wrong rate or channel deinterleave for its whole half).
    // The format TAG may differ (a plain-PCM recording vs an imported
    // WAVE_FORMAT_EXTENSIBLE of the same actual format), so compare those three
    // fields rather than byte-equal fmt chunks.
    if (!_compatibleForMerge(wa, wb)) {
      // Different formats can't be byte-spliced: a legacy 16-bit take beside a
      // 32-bit float recording, or a mono recording beside a stereo import
      // (or an earlier merge's output). Fall back to the engine's decode →
      // splice → re-encode path, which resamples both to the engine rate and
      // makes them stereo, with the split pinned at old's full duration (ceil,
      // so no old audio is dropped; at most <1 ms of pad). Output is a stereo
      // float WAV. Refusing a rate or channel mismatch outright made Merge
      // down fail for a recorded take under an imported one.
      final frames = wa.dataLen ~/ wa.blockAlign;
      final oldMs = (frames * 1000 + wa.sampleRate - 1) ~/ wa.sampleRate;
      final merged = await punchIn(
        practiceId: practiceId,
        oldPath: firstPath,
        newPath: secondPath,
        splitMs: oldMs,
      );
      if (merged == null) {
        throw const FormatException(
          'These takes have different audio formats and merging them by '
          're-encoding failed.',
        );
      }
      return merged;
    }
    // Trim each half DOWN to a whole number of frames before joining. A truncated
    // or malformed source can report a dataLen that isn't block-aligned, which would
    // make the second half begin mid-frame (channel swap + one-sample skew for its
    // entire span). A correctly-finalized WAV is always frame-aligned, so this is a
    // no-op for every well-formed file. blockAlign > 0 is guaranteed by the parse.
    final aLen = (wa.dataLen ~/ wa.blockAlign) * wa.blockAlign;
    final bLen = (wb.dataLen ~/ wb.blockAlign) * wb.blockAlign;
    // Classic RIFF sizes are u32: refuse a merge whose header could not cover
    // its own payload instead of writing one that silently lies past 4 GiB.
    // The budget counts both word-alignment pads too, since the RIFF size the
    // header carries is 20 + fmt (+ its pad) + data + pad.
    if (aLen + bLen + ((aLen + bLen).isOdd ? 1 : 0) >
        0xFFFFFFFF - 20 - wa.fmt.length - (wa.fmt.length.isOdd ? 1 : 0)) {
      throw const FormatException(
        'These takes are too long to merge into a single WAV file.',
      );
    }

    final path = await _newEditPath(practiceId: practiceId);
    await Isolate.run(() {
      // Unique temp + rename: the final path only ever holds a COMPLETE merge.
      final tmpPath = '$path.$pid.${DateTime.now().microsecondsSinceEpoch}.tmp';
      final out = File(tmpPath).openSync(mode: FileMode.write);
      var ok = false;
      try {
        _writeWavHeaderSync(out, wa.fmt, aLen + bLen);
        var copiedA = 0, copiedB = 0;
        for (final (src, start, len, isFirst) in [
          (firstPath, wa.dataStart, aLen, true),
          (secondPath, wb.dataStart, bLen, false),
        ]) {
          final raf = File(src).openSync();
          try {
            final copied = _copyRangeSync(raf, start, len, out);
            if (isFirst) {
              copiedA = copied;
            } else {
              copiedB = copied;
            }
          } finally {
            raf.closeSync();
          }
        }
        if (copiedA != aLen) {
          // A shortfall in the FIRST half shifts every frame of the second
          // half across the seam — that is corruption, not tolerance.
          throw const FormatException(
            'The first take changed on disk while merging.',
          );
        }
        if (copiedB == bLen) {
          if ((aLen + bLen).isOdd) out.writeByteSync(0);
        } else {
          // A short tail is safe to keep: it starts on a frame boundary.
          _patchWavSizesSync(
            out,
            wa.fmt,
            aLen + (copiedB ~/ wa.blockAlign) * wa.blockAlign,
          );
        }
        ok = true;
      } finally {
        out.closeSync();
        if (!ok) {
          try {
            File(tmpPath).deleteSync();
          } catch (_) {}
        }
      }
      File(tmpPath).renameSync(path);
    });
    return path;
  }

  /// Industry-standard import discipline: media becomes an engine-streamable
  /// WAV ONCE, at import — never decoded-in-full again at every project open.
  /// Returns [path] unchanged when the native feeder can stream it from disk
  /// as-is (see [isEngineStreamableWav]); otherwise transcodes it to an
  /// engine-rate float WAV (via the punch engine's streamed, transactional
  /// split-0 path — output streams to disk, lands atomically, and the decode
  /// budgets apply) and returns the NEW path. The caller owns deleting the
  /// original compressed copy.
  ///
  /// This is P0 of the disk-streaming migration (docs/audio-streaming-plan):
  /// once every track is engine-rate WAV, playback can move from full-RAM
  /// Arcs to file-backed feeders without a decoder in the loop.
  Future<String> ensureStreamableWav({
    required String practiceId,
    required String path,
  }) async {
    if (await isEngineStreamableWav(path)) return path;
    final out = await punchIn(
      practiceId: practiceId,
      // split == 0 keeps nothing of old and never reads it (pinned by test);
      // the source rides in as `new`.
      oldPath: path,
      newPath: path,
      splitMs: 0,
    );
    if (out == null) {
      throw const FormatException(
        'This file could not be converted for playback.',
      );
    }
    return out;
  }

  /// Whether the native feeder (`WavTrackReader::open` in feed.rs) can stream
  /// this file from disk: canonical 44-byte header (16-byte `fmt ` followed
  /// immediately by `data`), PCM16 or float32, 1-2 channels, engine rate.
  /// This MUST mirror the Rust admission exactly — the old check accepted ANY
  /// uncompressed WAV (44.1 kHz, EXTENSIBLE, PCM24, extra chunks), so those
  /// imports skipped the transcode, the feeder then refused them at commit,
  /// and the track silently fell back to the full-decode RAM path with none
  /// of the streaming migration's benefit. Pinned by mirrored test vectors
  /// here and in the engine's feed tests.
  static Future<bool> isEngineStreamableWav(String path) async {
    final WavProbe? probe;
    try {
      probe = await AudioFileService.probeWav(File(path));
    } on FileSystemException {
      // probeWav throws on an unreadable file so the recovery path can hold a
      // crashed take back rather than trash it. Here the question is only
      // "can the feeder stream this?", and "we cannot read it" answers no —
      // the caller's transcode path reports a proper failure of its own.
      return false;
    }
    if (probe == null) return false;
    return probe.sampleRate == _engineRate &&
        probe.channels >= 1 &&
        probe.channels <= 2;
  }

  /// Punch-in splice: produces a new WAV = `old[0..splitMs] ++ new`, where
  /// [splitMs] is the cursor offset from the START of [oldPath]'s audio. If the
  /// cursor lands past old's end the gap is silence-padded so [newPath] keeps
  /// its timeline position. Decode/splice/encode run in the Rust engine on a
  /// background isolate, so any source format works (output is engine-rate WAV).
  /// Returns the new file's path, or null on failure. Throws the typed
  /// refusal a decode names (too long, too dense, an unsupported rate, a read
  /// error): a file with no declared length past the hour was copied whole,
  /// then said only that it "could not be converted".
  Future<String?> punchIn({
    required String practiceId,
    required String oldPath,
    required String newPath,
    required int splitMs,
  }) async {
    final outPath = await _newEditPath(practiceId: practiceId);
    final result = await Isolate.run(() {
      final bindings = VocaraEngineBindings(_openVocaraLibrary());
      final oldC = oldPath.toNativeUtf8();
      final newC = newPath.toNativeUtf8();
      final outC = outPath.toNativeUtf8();
      try {
        return bindings.ve_punch_wav(
          oldC.cast(),
          newC.cast(),
          splitMs,
          _engineRate,
          outC.cast(),
        );
      } finally {
        malloc.free(oldC);
        malloc.free(newC);
        malloc.free(outC);
      }
    });
    if (result > 0) return outPath;
    final refused = decodeStatusToException(result, maxPlaybackDuration);
    if (refused != null) throw refused;
    return null;
  }

  /// The engine's working sample rate; punch-in output is written at this rate.
  static const int _engineRate = 48000;

  /// Copies an audio file (any format) to a fresh sandbox path, preserving its
  /// extension. Used by "Duplicate".
  Future<String> duplicate({
    required String practiceId,
    required String sourcePath,
  }) async {
    final ext = p.extension(sourcePath).replaceFirst('.', '');
    // Duplicates would dedup to an already-referenced content hash (so
    // recovery skips them anyway), but they go through the edits dir too:
    // one rule for every edit output beats a per-operation safety proof.
    final dest = await _newEditPath(
      practiceId: practiceId,
      extension: ext.isEmpty ? 'wav' : ext,
    );
    // Bounded, transactional copy (temp + rename, cleaned up on failure):
    // a half-copied duplicate must never sit at a final edits/ path.
    await AudioFileService.copyBounded(sourcePath, dest);
    return dest;
  }
}
