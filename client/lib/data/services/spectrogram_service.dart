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

  /// Header-only admission gate for the analysis paths: walks the WAV chunk
  /// list from a [RandomAccessFile] (seeking over chunk bodies, so a
  /// multi-gigabyte file costs a few hundred bytes of IO) and answers "should
  /// this file be analyzed at all?". Skips when:
  ///  * the file is not RIFF/WAVE — the WAV-only decoder would fail AFTER the
  ///    whole file loaded;
  ///  * no fmt/data chunk can be found — same reasoning;
  ///  * the declared PCM duration exceeds the analysis cap.
  /// Handles WAVE_FORMAT_EXTENSIBLE and extra chunks (LIST etc.), unlike the
  /// canonical-44-byte take probe — an imported WAV is exactly the file most
  /// likely to carry them.
  @visibleForTesting
  static Future<bool> shouldSkipAnalysis(File file) async {
    RandomAccessFile? raf;
    try {
      raf = await file.open();
      final head = await raf.read(12);
      if (head.length < 12) return true;
      String tag(Uint8List b, int o) => String.fromCharCodes(b, o, o + 4);
      if (tag(head, 0) != 'RIFF' || tag(head, 8) != 'WAVE') return true;
      var offset = 12;
      var sampleRate = 0, blockAlign = 0;
      // Bounded walk: a well-formed WAV reaches `data` within a handful of
      // chunks; 64 tolerates every real-world metadata pile-up while a
      // malformed chain can't spin forever.
      for (var i = 0; i < 64; i++) {
        await raf.setPosition(offset);
        final ch = await raf.read(8);
        if (ch.length < 8) return true; // ran out before a data chunk
        final id = tag(ch, 0);
        final size = ByteData.sublistView(ch).getUint32(4, Endian.little);
        if (id == 'fmt ') {
          final body = await raf.read(size < 40 ? size : 40);
          if (body.length < 16) return true;
          final bd = ByteData.sublistView(body);
          sampleRate = bd.getUint32(4, Endian.little);
          blockAlign = bd.getUint16(12, Endian.little);
        } else if (id == 'data') {
          if (sampleRate <= 0 || blockAlign <= 0) return true;
          final frames = size ~/ blockAlign;
          final capFrames =
              MediaAnalysisService.maxAnalysisDuration.inMilliseconds *
              sampleRate ~/
              1000;
          return frames > capFrames;
        }
        offset += 8 + size + (size & 1); // chunks are word-aligned
      }
      return true;
    } catch (_) {
      // Unreadable now → unreadable in the isolate too; don't load it there.
      return true;
    } finally {
      await raf?.close();
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
    if (await shouldSkipAnalysis(file)) return null;
    // STFT is heavy — run it in a background isolate (which also reads the
    // bytes, so they never exist twice).
    return Isolate.run(() {
      final pcm = readWavMono(File(path).readAsBytesSync());
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
    if (await shouldSkipAnalysis(file)) return null;
    return Isolate.run(() {
      final pcm = readWavMono(File(path).readAsBytesSync());
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
    if (await shouldSkipAnalysis(file)) return null;
    return Isolate.run(() {
      final pcm = readWavMono(File(path).readAsBytesSync());
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
