import 'dart:isolate';
import 'dart:typed_data';

import '../../domain/models/technique_analysis.dart';
import '../../domain/models/voice_metrics.dart';
import '../../utils/analysis_gate.dart';
import 'audio_dsp.dart';
import 'graph_audio_playback_service.dart'
    show MediaTooLongException, decodeMediaToMono;

/// Computes the full take-style analysis bundle for an arbitrary media file (any
/// format the engine can decode), for the technique library's reference "ground
/// truth" and the ephemeral practice comparison. The heavy DSP runs off the UI
/// thread.
class MediaAnalysisService {
  const MediaAnalysisService();

  /// Longest media the analysis path will decode. The whole file materializes
  /// as Float32 PCM and is copied again across the isolate boundaries (see
  /// [decodeMediaToMono]), so an unbounded input could exhaust memory.
  /// 10 minutes: takes and technique references are minutes long.
  ///
  /// The ≈ 86 MB figure measured in test/measure_analysis_peak_test.dart is
  /// the WAV path's (SpectrogramService's windowed StreamingTakeAnalyzer,
  /// which never materializes the track) — NOT this one. THIS path decodes
  /// through the native engine, which holds the whole track, and then runs
  /// BATCH DSP over it, so its peak is dominated by the track itself: ~4 bytes
  /// per frame native (the downmix happens inside that buffer, so stereo and
  /// mono never coexist), the same again for the Dart copy the isolate hop
  /// needs, plus the results. A VM test cannot load the engine framework, so
  /// this stays a derivation; the duration cap is what bounds it, and the
  /// global analysis gate's two slots bound how many can stack.
  ///
  /// Routing this path through the same windowed analyzer — a chunked native
  /// decode feeding StreamingTakeAnalyzer — is what would remove the
  /// whole-track term; the per-frame math is already pinned bit-identical to
  /// the batch functions (test/voice_metrics_test.dart), so the analyzer is
  /// not the obstacle, the decode's shape is.
  static const Duration maxAnalysisDuration = Duration(minutes: 10);

  /// Decodes [path] and runs the complete analysis suite. Returns null when the
  /// file can't be decoded or is empty; throws [MediaTooLongException] when it
  /// exceeds [maxAnalysisDuration] (a clear, user-presentable message —
  /// callers already handle analyze() throwing).
  Future<TechniqueAnalysis?> analyze(String path) async {
    // Gated end to end: the DECODE is the larger peak of this pipeline, so
    // it must sit inside the same admission as the analysis isolate —
    // otherwise two callers could stack full-track decodes while "only"
    // their analyses were serialized.
    return analysisGate.run(() async {
      final pcm = await decodeMediaToMono(
        path,
        maxDuration: maxAnalysisDuration,
      );
      if (pcm == null || pcm.samples.isEmpty) return null;
      final samples = pcm.samples;
      final rate = pcm.sampleRate;
      return Isolate.run(() => _analyze(samples, rate));
    });
  }

  static TechniqueAnalysis? _analyze(Float32List samples, int sampleRate) {
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
