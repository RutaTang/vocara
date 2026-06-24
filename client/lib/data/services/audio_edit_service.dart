import 'dart:ffi' as ffi;
import 'dart:io';
import 'dart:isolate';
import 'dart:math' as math;
import 'dart:typed_data';

import 'package:ffi/ffi.dart';
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
  return _WavLayout(fmt, sampleRate, blockAlign, dataStart, dataLen);
}

/// Re-emits a WAV from a preserved `fmt ` chunk body and raw PCM [data] bytes.
/// Whether two WAVs can be concatenated losslessly under one fmt header: same
/// sample rate, channel count, and bit depth. The format tag may differ (plain
/// PCM vs WAVE_FORMAT_EXTENSIBLE of the same actual format) since the PCM bytes
/// are interpreted identically. Channels live at fmt offset 2, bits at 14.
bool _compatibleForMerge(_WavLayout a, _WavLayout b) {
  int u16(Uint8List f, int off) =>
      off + 1 < f.length ? f[off] | (f[off + 1] << 8) : -1;
  // The effective sample TYPE: for WAVE_FORMAT_EXTENSIBLE (0xFFFE) it's the SubFormat
  // GUID's leading format code at fmt offset 24; otherwise the format tag at offset 0
  // (1 = integer PCM, 3 = IEEE float). Comparing this — not just rate/channels/bits —
  // stops a 32-bit int WAV and a 32-bit float WAV (same rate/channels/depth) from merging
  // into reinterpreted noise, while still allowing plain PCM vs EXTENSIBLE-PCM of the same
  // actual format (both resolve to 1).
  int effFmt(Uint8List f) {
    final tag = u16(f, 0);
    return tag == 0xFFFE ? u16(f, 24) : tag;
  }

  return a.sampleRate == b.sampleRate &&
      u16(a.fmt, 2) == u16(b.fmt, 2) &&
      u16(a.fmt, 14) == u16(b.fmt, 14) &&
      effFmt(a.fmt) == effFmt(b.fmt);
}

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

/// Non-destructive clip edits on WAV files. Split/merge slice and join the raw
/// PCM **bytes** (preserving the source's exact sample format — 16/24/32-bit,
/// integer or float, plain or WAVE_FORMAT_EXTENSIBLE), so any uncompressed WAV
/// works losslessly. Recordings are WAV, so takes can always be edited.
class AudioEditService {
  AudioEditService({required AudioFileService files}) : _files = files;

  final AudioFileService _files;

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

    final firstPath = await _files.newRecordingPath(practiceId: practiceId);
    final secondPath = await _files.newRecordingPath(practiceId: practiceId);
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
    // fields rather than byte-equal fmt chunks. Reject real mismatches loudly.
    if (!_compatibleForMerge(wa, wb)) {
      throw const FormatException(
        'These takes have different audio formats (sample rate, channels, or '
        'bit depth), so they can\'t be merged without re-encoding.',
      );
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

    final path = await _files.newRecordingPath(practiceId: practiceId);
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
    final outPath = await _files.newRecordingPath(practiceId: practiceId);
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
    final dest = await _files.newRecordingPath(
      practiceId: practiceId,
      extension: ext.isEmpty ? 'wav' : ext,
    );
    await File(sourcePath).copy(dest);
    return dest;
  }
}
