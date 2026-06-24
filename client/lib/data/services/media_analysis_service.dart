import 'dart:isolate';
import 'dart:typed_data';

import '../../domain/models/technique_analysis.dart';
import '../../domain/models/voice_metrics.dart';
import 'audio_dsp.dart';
import 'graph_audio_playback_service.dart' show decodeMediaToMono;

/// Computes the full take-style analysis bundle for an arbitrary media file (any
/// format the engine can decode), for the technique library's reference "ground
/// truth" and the ephemeral practice comparison. The heavy DSP runs off the UI
/// thread.
class MediaAnalysisService {
  const MediaAnalysisService();

  /// Decodes [path] and runs the complete analysis suite. Returns null when the
  /// file can't be decoded or is empty.
  Future<TechniqueAnalysis?> analyze(String path) async {
    final pcm = await decodeMediaToMono(path);
    if (pcm == null || pcm.samples.isEmpty) return null;
    final samples = pcm.samples;
    final rate = pcm.sampleRate;
    return Isolate.run(() => _analyze(samples, rate));
  }

  static TechniqueAnalysis? _analyze(Float64List samples, int sampleRate) {
    if (samples.isEmpty || sampleRate <= 0) return null;
    final metrics = computeVoiceMetrics(samples, sampleRate);
    final spectrogram = computeSpectrogram(samples, sampleRate);
    return TechniqueAnalysis(
      metrics: metrics,
      spectrogram: spectrogram,
      ltas: computeLtas(spectrogram, metrics),
      profile: analyzeVoiceProfile(metrics),
      register: analyzeRegister(metrics),
      durationMs: (samples.length / sampleRate * 1000).round(),
    );
  }
}
