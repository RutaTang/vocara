import 'spectrogram.dart';
import 'voice_metrics.dart';

/// A complete offline analysis of one audio clip — the same suite a vocal take
/// gets (per-frame timbre metrics, spectrogram, long-term average spectrum,
/// voice profile and register map).
///
/// Used two ways in the technique library: as a technique's reference "ground
/// truth" (computed from its example media), and for an ephemeral practice
/// recording compared against that reference. Immutable; produced by
/// `MediaAnalysisService`.
class TechniqueAnalysis {
  const TechniqueAnalysis({
    required this.metrics,
    required this.spectrogram,
    required this.ltas,
    required this.profile,
    required this.register,
    required this.durationMs,
  });

  /// Per-frame timbre metrics (F0/H1−H2/ring/tilt/CPP/formants).
  final VoiceMetrics metrics;

  /// Time–frequency magnitude image.
  final Spectrogram spectrogram;

  /// Long-term average spectrum (the clip's overall spectral shape).
  final Ltas ltas;

  /// Range / passaggio / dynamics summary derived from [metrics].
  final VoiceProfile profile;

  /// Chest / mix / head register classification over the clip.
  final RegisterMap register;

  /// Clip length in milliseconds.
  final int durationMs;
}
