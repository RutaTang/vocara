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
  /// 10 minutes: takes and technique references are minutes long. MEASURED
  /// (test/measure_analysis_peak_test.dart, RSS delta while computeBoth runs
  /// on the max legal 10-minute 48 kHz float WAV): ≈ 86 MB — the WINDOWED
  /// streaming pipeline (StreamingTakeAnalyzer) holds a few analysis windows
  /// beside the RESULTS (spectrogram columns, per-metric arrays, the
  /// cross-isolate return copy); the whole-track sample buffer and the full
  /// decimated copies no longer exist. Down from ≈ 400 MB (Float32 batch)
  /// and ≈ 700 MB (the original Float64 batch). The arbitrary-media path
  /// through the NATIVE decoder still materializes full PCM on its own side
  /// (unmeasurable in VM tests, which cannot load the engine framework);
  /// this cap bounds it. The previous 30-minute value predated any memory
  /// budgeting.
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
