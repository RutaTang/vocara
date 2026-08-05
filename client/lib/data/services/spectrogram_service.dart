import 'dart:io';
import 'dart:isolate';

import 'package:flutter/foundation.dart';

import '../../domain/models/spectrogram.dart';
import '../../domain/models/voice_metrics.dart';
import 'audio_dsp.dart';
import 'media_analysis_service.dart';

/// Spectrogram + voice metrics computed from ONE decode of the same file — see
/// [SpectrogramService.computeBothFromFile].
typedef TakeAnalysis = ({Spectrogram? spectrogram, VoiceMetrics? metrics});

/// Everything the admission gate learned from the header, handed to the
/// streaming reader so the isolate never re-walks (or re-trusts) the chunks:
/// where the PCM lives, its layout, and the effective sample format
/// (1 = integer PCM, 3 = IEEE float — extensible resolved to its SubFormat).
typedef WavAdmission = ({
  int dataStart,
  int frames,
  int sampleRate,
  int channels,
  int bits,
  int format,
});

/// Computes spectrograms from audio files off the UI thread. v1 supports
/// uncompressed WAV (our recordings + dropped WAV); returns null for other
/// formats.
///
/// Memory discipline: the file is read INSIDE the analysis isolate (only the
/// path crosses the boundary), so the raw bytes exist once — passing a
/// `Uint8List` into `Isolate.run` would deep-copy it, doubling the resident
/// bytes for the whole compute. Files longer than
/// [MediaAnalysisService.maxAnalysisDuration] — and files that are not WAV at
/// all — are refused by a header-only probe BEFORE anything allocates: the
/// old behavior loaded the entire file just to have the decoder say no.
class SpectrogramService {
  const SpectrogramService();

  /// Absolute ceilings for the admission gate, INDEPENDENT of the header's
  /// arithmetic — a forged header must not be able to talk its way past them.
  /// Derivation: the legal upper bound the analysis is designed for is a
  /// 30-minute 48 kHz stereo float32 WAV, i.e. 30*60*48000 = 86.4 M frames
  /// and ~691 MB of PCM. Frames matter separately from bytes because the
  /// decoder expands to Float64 MONO (frames * 8 bytes): a 1 GiB 8-bit mono
  /// file is only ~1 GiB on disk but ~8 GiB decoded, so a byte ceiling alone
  /// is not a memory bound.
  @visibleForTesting
  static const int maxAnalysisFrames = 90 * 1000 * 1000;
  @visibleForTesting
  static const int maxAnalysisBytes = 720 * 1000 * 1000;

  /// Header-only admission gate for the analysis paths: walks the WAV chunk
  /// list from a [RandomAccessFile] (seeking over chunk bodies, so a
  /// multi-gigabyte file costs a few hundred bytes of IO) and answers "should
  /// this file be analyzed, and how many bytes of it?". Returns null (skip)
  /// when:
  ///  * the file is not RIFF/WAVE — the WAV-only decoder would fail AFTER the
  ///    whole file loaded;
  ///  * no fmt/data chunk can be found — same reasoning;
  ///  * the declared PCM duration exceeds the analysis cap.
  /// Otherwise returns the byte offset where the data chunk ENDS: the analysis
  /// isolate reads exactly that much, so a small declared data chunk with
  /// gigabytes of trailing junk (appended metadata, garbage, a lying tail)
  /// can no longer ride into memory on the strength of an honest header.
  /// Handles WAVE_FORMAT_EXTENSIBLE and extra chunks (LIST etc.), unlike the
  /// canonical-44-byte take probe — an imported WAV is exactly the file most
  /// likely to carry them.
  @visibleForTesting
  static Future<WavAdmission?> admitForAnalysis(File file) async {
    RandomAccessFile? raf;
    try {
      raf = await file.open();
      final head = await raf.read(12);
      if (head.length < 12) return null;
      String tag(Uint8List b, int o) => String.fromCharCodes(b, o, o + 4);
      if (tag(head, 0) != 'RIFF' || tag(head, 8) != 'WAVE') return null;
      var offset = 12;
      var sampleRate = 0, blockAlign = 0, channels = 0, bits = 0, format = 0;
      // Bounded walk: a well-formed WAV reaches `data` within a handful of
      // chunks; 64 tolerates every real-world metadata pile-up while a
      // malformed chain can't spin forever.
      for (var i = 0; i < 64; i++) {
        await raf.setPosition(offset);
        final ch = await raf.read(8);
        if (ch.length < 8) return null; // ran out before a data chunk
        final id = tag(ch, 0);
        final size = ByteData.sublistView(ch).getUint32(4, Endian.little);
        if (id == 'fmt ') {
          final body = await raf.read(size < 40 ? size : 40);
          if (body.length < 16) return null;
          final bd = ByteData.sublistView(body);
          sampleRate = bd.getUint32(4, Endian.little);
          blockAlign = bd.getUint16(12, Endian.little);
          // The gate's frame count comes from size / blockAlign, so a FORGED
          // blockAlign is exactly how a huge file talks its frame count down
          // under the duration cap. Same admission rules as the edit
          // service's parser: an uncompressed blockAlign is exactly
          // channels * ceil(bits / 8), and the supported envelope is bounded
          // (channels, bit depth, rate) so no header arithmetic can inflate
          // the ceilings below.
          channels = bd.getUint16(2, Endian.little);
          bits = bd.getUint16(14, Endian.little);
          // Effective sample format: WAVE_FORMAT_EXTENSIBLE (0xFFFE) resolves
          // to the SubFormat GUID's leading code at fmt offset 24.
          final tag2 = bd.getUint16(0, Endian.little);
          format = tag2 == 0xFFFE && body.length >= 26
              ? bd.getUint16(24, Endian.little)
              : tag2;
          const legalBits = {8, 16, 24, 32, 64};
          final legalFormat = switch (format) {
            1 => legalBits.contains(bits),
            3 => bits == 32 || bits == 64,
            _ => false,
          };
          if (channels < 1 ||
              channels > 8 ||
              !legalFormat ||
              sampleRate < 8000 ||
              sampleRate > 384000 ||
              blockAlign != channels * ((bits + 7) ~/ 8)) {
            return null;
          }
        } else if (id == 'data') {
          if (sampleRate <= 0 || blockAlign <= 0 || format == 0) return null;
          // Ceilings judge the DECLARED size first — the whole point of the
          // gate is refusing a big file from its header, before any physical
          // size is consulted.
          final declaredFrames = size ~/ blockAlign;
          final capFrames =
              MediaAnalysisService.maxAnalysisDuration.inMilliseconds *
              sampleRate ~/
              1000;
          if (declaredFrames > capFrames) return null;
          // Header-independent ceilings (see their doc): bytes bound what the
          // reader streams through, frames bound the Float64 analysis buffer.
          if (declaredFrames > maxAnalysisFrames) return null;
          if (offset + 8 + size > maxAnalysisBytes) return null;
          // Only THEN clamp to what the file physically holds (a hard-killed
          // or lying header must not make the reader wait on missing bytes).
          final fileLen = await file.length();
          final avail = fileLen - (offset + 8);
          if (avail <= 0) return null;
          final dataLen = size < avail ? size : avail;
          final frames = dataLen ~/ blockAlign;
          if (frames == 0) return null;
          return (
            dataStart: offset + 8,
            frames: frames,
            sampleRate: sampleRate,
            channels: channels,
            bits: bits,
            format: format,
          );
        }
        offset += 8 + size + (size & 1); // chunks are word-aligned
      }
      return null;
    } catch (_) {
      // Unreadable now → unreadable in the isolate too; don't load it there.
      return null;
    } finally {
      await raf?.close();
    }
  }

  /// Streams the admitted PCM straight into the ONE Float64 mono analysis
  /// buffer, in bounded frame-aligned chunks (4 MiB). Runs inside the
  /// analysis isolate. This is the memory-budget half of the gate: the old
  /// shape read the whole byte payload first and THEN decoded, so a legal
  /// 30-minute stereo float take held ~691 MB of bytes beside ~691 MB of
  /// Float64 — the peak is now the analysis buffer plus one chunk.
  @visibleForTesting
  static PcmAudio? readWavMonoStreaming(String path, WavAdmission a) {
    final sampleBytes = (a.bits + 7) ~/ 8;
    final frameBytes = a.channels * sampleBytes;
    final out = Float64List(a.frames);
    final raf = File(path).openSync();
    try {
      raf.setPositionSync(a.dataStart);
      const chunkBudget = 4 << 20;
      final framesPerChunk = (chunkBudget ~/ frameBytes).clamp(1, a.frames);
      var frame = 0;
      final inv = 1.0 / a.channels;
      while (frame < a.frames) {
        final want = (a.frames - frame).clamp(0, framesPerChunk);
        final bytes = raf.readSync(want * frameBytes);
        final got = bytes.length ~/ frameBytes;
        if (got == 0) break; // truncated on disk: keep what exists
        final bd = ByteData.sublistView(bytes);
        for (var f = 0; f < got; f++) {
          var acc = 0.0;
          final base = f * frameBytes;
          for (var c = 0; c < a.channels; c++) {
            final o = base + c * sampleBytes;
            acc += switch ((a.format, a.bits)) {
              (1, 8) => (bytes[o] - 128) / 128.0,
              (1, 16) => bd.getInt16(o, Endian.little) / 32768.0,
              (1, 24) =>
                (bytes[o] | (bytes[o + 1] << 8) | (bytes[o + 2] << 16))
                        .toSigned(24) /
                    8388608.0,
              (1, 32) => bd.getInt32(o, Endian.little) / 2147483648.0,
              (3, 32) => bd.getFloat32(o, Endian.little),
              (3, 64) => bd.getFloat64(o, Endian.little),
              _ => 0.0, // unreachable: the gate admitted the format
            };
          }
          out[frame + f] = acc * inv;
        }
        frame += got;
      }
      if (frame == 0) return null;
      // A truncated file yields a shorter buffer; keep the honest length.
      return PcmAudio(
        frame == a.frames ? out : Float64List.sublistView(out, 0, frame),
        a.sampleRate,
      );
    } finally {
      raf.closeSync();
    }
  }

  Future<Spectrogram?> computeFromFile(
    String path, {
    int fftSize = 2048,
    int hop = 512,
    double maxHz = 5000,
  }) async {
    final file = File(path);
    if (!await file.exists()) return null;
    final admission = await admitForAnalysis(file);
    if (admission == null) return null;
    // STFT is heavy — run it in a background isolate (which also streams the
    // decode, so raw bytes never materialize whole).
    return Isolate.run(() {
      final pcm = readWavMonoStreaming(path, admission);
      if (pcm == null) return null;
      return computeSpectrogram(
        pcm.samples,
        pcm.sampleRate,
        fftSize: fftSize,
        hop: hop,
        maxHz: maxHz,
      );
    });
  }

  /// Computes per-frame voice-timbre metrics (ring/tilt/CPP/formants) for a take
  /// off the UI thread. Same WAV/PCM16-only support as [computeFromFile].
  Future<VoiceMetrics?> computeMetricsFromFile(
    String path, {
    int fftSize = 2048,
    int hop = 512,
  }) async {
    final file = File(path);
    if (!await file.exists()) return null;
    final admission = await admitForAnalysis(file);
    if (admission == null) return null;
    return Isolate.run(() {
      final pcm = readWavMonoStreaming(path, admission);
      if (pcm == null) return null;
      return computeVoiceMetrics(
        pcm.samples,
        pcm.sampleRate,
        fftSize: fftSize,
        hop: hop,
      );
    });
  }

  /// Computes the spectrogram AND the voice metrics from a single decode, in a
  /// single isolate. The two analyses always run together when a take is
  /// selected; computing them separately used to decode the same file twice in
  /// two concurrent isolates — twice the bytes, twice the PCM, at the same
  /// moment. Null when the file is missing or over the analysis cap; each
  /// field is null when its compute failed.
  Future<TakeAnalysis?> computeBothFromFile(
    String path, {
    int fftSize = 2048,
    int hop = 512,
    double maxHz = 5000,
  }) async {
    final file = File(path);
    if (!await file.exists()) return null;
    final admission = await admitForAnalysis(file);
    if (admission == null) return null;
    return Isolate.run(() {
      final pcm = readWavMonoStreaming(path, admission);
      if (pcm == null) return null;
      final spectrogram = computeSpectrogram(
        pcm.samples,
        pcm.sampleRate,
        fftSize: fftSize,
        hop: hop,
        maxHz: maxHz,
      );
      final metrics = computeVoiceMetrics(
        pcm.samples,
        pcm.sampleRate,
        fftSize: fftSize,
        hop: hop,
      );
      return (spectrogram: spectrogram, metrics: metrics);
    });
  }
}
