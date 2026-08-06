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
  /// as Float32 PCM and is copied again across the isolate boundaries (see
  /// [decodeMediaToMono]), so an unbounded input could exhaust memory.
  /// 10 minutes: takes and technique references are minutes long, and the
  /// analysis pipeline's peak memory scales linearly with this. The legal
  /// worst case at this cap is roughly 350 MB for computeBoth (the Float32
  /// mono buffer + the decimation working copy at ~8 bytes/frame, plus the
  /// spectrogram result, per-metric arrays, and the cross-isolate return
  /// copy) — halved from ~700 MB by the Float32 sample-buffer migration
  /// (sources are 16/24/32-bit integer or float32, all exactly representable;
  /// windowed math still runs in doubles). Further reduction means windowed
  /// streaming DSP, tracked separately. The previous 30-minute value predated
  /// any memory budgeting.
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
