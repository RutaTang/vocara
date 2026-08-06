import 'dart:ffi' as ffi;
import 'dart:io';
import 'dart:isolate';
import 'dart:math' as math;
import 'dart:typed_data';

import 'package:ffi/ffi.dart';
import 'package:flutter/foundation.dart' show visibleForTesting;
import 'package:path/path.dart' as p;

import '../ffi/vocara_engine_bindings.dart';
import 'audio_file_service.dart';

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
  final bothExtensible = a.fmt.length >= 40 && b.fmt.length >= 40;
  if (bothExtensible && mask(a) != mask(b)) return false;
  return true;
}

/// Re-emits a WAV from a preserved `fmt ` chunk body and raw PCM [data] bytes.
Uint8List _buildWav(Uint8List fmt, Uint8List data) {
  final b = BytesBuilder();
  void str(String s) => b.add(s.codeUnits);
  void u32(int v) =>
      b.add((ByteData(4)..setUint32(0, v, Endian.little)).buffer.asUint8List());
  final pad = data.length.isOdd ? 1 : 0;
  final riffSize = 4 + (8 + fmt.length) + (8 + data.length + pad);
  str('RIFF');
  u32(riffSize);
  str('WAVE');
  str('fmt ');
  u32(fmt.length);
  b.add(fmt);
  str('data');
  u32(data.length);
  b.add(data);
  if (pad == 1) b.addByte(0);
  return b.toBytes();
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
  await Isolate.run(() => _trimWavHeadSync(path, trimMs));
}

/// Isolate body of [trimWavHeadInPlace]: RandomAccessFile chunk walk (header
/// bytes only), then a chunked data copy into the temp file.
void _trimWavHeadSync(String path, int trimMs) {
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

    final tmpPath = '$path.trim';
    final out = File(tmpPath).openSync(mode: FileMode.write);
    try {
      final header = BytesBuilder();
      void str(String s) => header.add(s.codeUnits);
      void u32(int v) => header.add(
        (ByteData(4)..setUint32(0, v, Endian.little)).buffer.asUint8List(),
      );
      final pad = newLen.isOdd ? 1 : 0;
      str('RIFF');
      u32(4 + (8 + f.length) + (8 + newLen + pad));
      str('WAVE');
      str('fmt ');
      u32(f.length);
      header.add(f);
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
      if (pad == 1) out.writeByteSync(0);
    } finally {
      out.closeSync();
    }
    File(tmpPath).renameSync(path);
  } finally {
    raf.closeSync();
  }
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
    final bytes = await File(sourcePath).readAsBytes();
    final w = _parseWav(bytes);
    if (w == null) {
      throw const FormatException(
        'This take is compressed audio (e.g. mp3/m4a). Only recordings and '
        'WAV files can be split.',
      );
    }
    // Need at least one frame on each side. A 0-length / sub-2-frame take (e.g. a
    // recording quit mid-capture) would otherwise give clamp bounds where lower >
    // upper, and num.clamp throws an opaque ArgumentError instead of this message.
    if (w.dataLen < 2 * w.blockAlign) {
      throw const FormatException('This take is too short to split.');
    }
    final frame = (splitFileMs / 1000 * w.sampleRate).round();
    final cut = (frame * w.blockAlign).clamp(
      w.blockAlign,
      w.dataLen - w.blockAlign,
    );
    final data = Uint8List.sublistView(
      bytes,
      w.dataStart,
      w.dataStart + w.dataLen,
    );

    final firstPath = await _newEditPath(practiceId: practiceId);
    final secondPath = await _newEditPath(practiceId: practiceId);
    await File(
      firstPath,
    ).writeAsBytes(_buildWav(w.fmt, Uint8List.sublistView(data, 0, cut)));
    await File(
      secondPath,
    ).writeAsBytes(_buildWav(w.fmt, Uint8List.sublistView(data, cut)));
    return (firstPath: firstPath, secondPath: secondPath);
  }

  /// Concatenates two WAVs end-to-end ([firstPath] then [secondPath]) into one
  /// new WAV (inverse of [splitWav]). Both must share a sample format.
  Future<String> concatWav({
    required String practiceId,
    required String firstPath,
    required String secondPath,
  }) async {
    final ab = await File(firstPath).readAsBytes();
    final bb = await File(secondPath).readAsBytes();
    final wa = _parseWav(ab);
    final wb = _parseWav(bb);
    if (wa == null || wb == null) {
      throw const FormatException(
        'These takes are compressed audio. Only recordings and WAV files '
        'can be merged.',
      );
    }
    // The output reuses wa's fmt header for BOTH halves, so the takes must share
    // the fields that determine how the PCM bytes are read — sample rate,
    // channels, bit depth — otherwise the second take is reinterpreted under the
    // first's format (wrong rate or channel deinterleave for its whole half).
    // The format TAG may differ (a plain-PCM recording vs an imported
    // WAVE_FORMAT_EXTENSIBLE of the same actual format), so compare those three
    // fields rather than byte-equal fmt chunks.
    if (!_compatibleForMerge(wa, wb)) {
      final sameLayout =
          wa.sampleRate == wb.sampleRate &&
          _fmtU16(wa.fmt, 2) == _fmtU16(wb.fmt, 2);
      if (!sameLayout) {
        throw const FormatException(
          'These takes have different audio formats (sample rate or '
          'channels), so they can\'t be merged without re-encoding.',
        );
      }
      // Same rate/channels, different sample format: the seam between a legacy
      // 16-bit take and a new 32-bit float recording. Byte-splicing is
      // impossible, so fall back to the engine's decode → splice → re-encode
      // path with the split pinned at old's full duration (ceil, so no old
      // audio is dropped; at most <1 ms of pad). Output is a float WAV.
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
          'These takes have different sample formats and merging them by '
          're-encoding failed.',
        );
      }
      return merged;
    }
    // Trim each half DOWN to a whole number of frames before joining. A truncated
    // or malformed source can report a dataLen that isn't block-aligned, which would
    // make the second half begin mid-frame (channel swap + one-sample skew for its
    // entire span). A correctly-finalized WAV is always frame-aligned, so this is a
    // no-op for every well-formed file. blockAlign > 0 is guaranteed by _parseWav.
    final aLen = (wa.dataLen ~/ wa.blockAlign) * wa.blockAlign;
    final bLen = (wb.dataLen ~/ wb.blockAlign) * wb.blockAlign;
    final da = Uint8List.sublistView(ab, wa.dataStart, wa.dataStart + aLen);
    final db = Uint8List.sublistView(bb, wb.dataStart, wb.dataStart + bLen);
    final merged = Uint8List(da.length + db.length)
      ..setRange(0, da.length, da)
      ..setRange(da.length, da.length + db.length, db);

    final path = await _newEditPath(practiceId: practiceId);
    await File(path).writeAsBytes(_buildWav(wa.fmt, merged));
    return path;
  }

  /// Punch-in splice: produces a new WAV = `old[0..splitMs] ++ new`, where
  /// [splitMs] is the cursor offset from the START of [oldPath]'s audio. If the
  /// cursor lands past old's end the gap is silence-padded so [newPath] keeps
  /// its timeline position. Decode/splice/encode run in the Rust engine on a
  /// background isolate, so any source format works (output is engine-rate WAV).
  /// Returns the new file's path, or null on failure.
  Future<String?> punchIn({
    required String practiceId,
    required String oldPath,
    required String newPath,
    required int splitMs,
  }) async {
    final outPath = await _newEditPath(practiceId: practiceId);
    final ok = await Isolate.run(() {
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
            ) >
            0;
      } finally {
        malloc.free(oldC);
        malloc.free(newC);
        malloc.free(outC);
      }
    });
    return ok ? outPath : null;
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
    await File(sourcePath).copy(dest);
    return dest;
  }
}
