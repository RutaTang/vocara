import 'dart:io';
import 'dart:isolate';

import '../../domain/models/spectrogram.dart';
import '../../domain/models/voice_metrics.dart';
import 'audio_dsp.dart';
import 'audio_file_service.dart';
import 'media_analysis_service.dart';

/// Spectrogram + voice metrics computed from ONE decode of the same file — see
/// [SpectrogramService.computeBothFromFile].
typedef TakeAnalysis = ({Spectrogram? spectrogram, VoiceMetrics? metrics});

/// Computes spectrograms from audio files off the UI thread. v1 supports 16-bit
/// PCM WAV (our recordings + dropped WAV); returns null for other formats.
///
/// Memory discipline: the file is read INSIDE the analysis isolate (only the
/// path crosses the boundary), so the raw bytes exist once — passing a
/// `Uint8List` into `Isolate.run` would deep-copy it, doubling the resident
/// bytes for the whole compute. Files longer than
/// [MediaAnalysisService.maxAnalysisDuration] are refused up front (header
/// probe, no allocation): a full-length decode of an arbitrary import is an
/// OOM, not an analysis.
class SpectrogramService {
  const SpectrogramService();

  /// Whether the WAV at [path] is longer than the analysis cap. Best-effort:
  /// only canonical takes probe successfully; anything else returns false and
  /// is judged by the decoder as before.
  static Future<bool> _exceedsAnalysisCap(File file) async {
    final probe = await AudioFileService.probeWav(file);
    if (probe == null) return false;
    final bytesPerFrame = probe.channels * (probe.bitsPerSample ~/ 8);
    if (bytesPerFrame <= 0) return false;
    final frames = probe.dataBytes ~/ bytesPerFrame;
    final capFrames =
        MediaAnalysisService.maxAnalysisDuration.inMilliseconds *
        probe.sampleRate ~/
        1000;
    return frames > capFrames;
  }

  Future<Spectrogram?> computeFromFile(
    String path, {
    int fftSize = 2048,
    int hop = 512,
    double maxHz = 5000,
  }) async {
    final file = File(path);
    if (!await file.exists()) return null;
    if (await _exceedsAnalysisCap(file)) return null;
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
    if (await _exceedsAnalysisCap(file)) return null;
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
    if (await _exceedsAnalysisCap(file)) return null;
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
