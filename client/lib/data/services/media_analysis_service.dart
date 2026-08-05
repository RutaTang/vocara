import 'dart:isolate';
import 'dart:typed_data';

import '../../domain/models/technique_analysis.dart';
import '../../domain/models/voice_metrics.dart';
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
  /// as Float64 PCM and is copied again across the isolate boundaries (see
  /// [decodeMediaToMono]), so an unbounded input could exhaust memory.
  /// 30 minutes covers any realistic technique reference or practice take
  /// while bounding the transient allocations to a few hundred MB.
  /// 10 minutes: takes and technique references are minutes long, and the
  /// analysis pipeline's peak memory scales linearly with this (the Float64
  /// mono buffer plus the decimation working copy — roughly 16 bytes per
  /// frame, ~460 MB at this cap). The previous 30-minute value predated the
  /// memory budgeting and put the legal worst case well past 1 GB.
  static const Duration maxAnalysisDuration = Duration(minutes: 10);

  /// Decodes [path] and runs the complete analysis suite. Returns null when the
  /// file can't be decoded or is empty; throws [MediaTooLongException] when it
  /// exceeds [maxAnalysisDuration] (a clear, user-presentable message —
  /// callers already handle analyze() throwing).
  Future<TechniqueAnalysis?> analyze(String path) async {
    final pcm = await decodeMediaToMono(path, maxDuration: maxAnalysisDuration);
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
