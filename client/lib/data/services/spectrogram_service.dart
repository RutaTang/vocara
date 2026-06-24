import 'dart:io';
import 'dart:isolate';

import '../../domain/models/spectrogram.dart';
import '../../domain/models/voice_metrics.dart';
import 'audio_dsp.dart';

/// Computes spectrograms from audio files off the UI thread. v1 supports 16-bit
/// PCM WAV (our recordings + dropped WAV); returns null for other formats.
class SpectrogramService {
  const SpectrogramService();

  Future<Spectrogram?> computeFromFile(
    String path, {
    int fftSize = 2048,
    int hop = 512,
    double maxHz = 5000,
  }) async {
    final file = File(path);
    if (!await file.exists()) return null;
    final bytes = await file.readAsBytes();
    // STFT is heavy — run it in a background isolate.
    return Isolate.run(() {
      final pcm = readWavMono(bytes);
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
    final bytes = await file.readAsBytes();
    return Isolate.run(() {
      final pcm = readWavMono(bytes);
      if (pcm == null) return null;
      return computeVoiceMetrics(
        pcm.samples,
        pcm.sampleRate,
        fftSize: fftSize,
        hop: hop,
      );
    });
  }
}
