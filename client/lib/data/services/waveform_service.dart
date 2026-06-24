import 'dart:io';

import 'package:just_waveform/just_waveform.dart';
import 'package:path/path.dart' as p;

import '../../utils/id.dart';
import 'local_storage_service.dart';

/// Result of analyzing an audio file: normalized peaks plus its duration.
typedef WaveformResult = ({List<double> peaks, int durationMs});

/// Extracts a renderable waveform from an audio file using just_waveform
/// (AVFoundation on macOS), then down-samples it to a compact, normalized peak
/// list that is stored on the track and drawn by a CustomPainter.
class WaveformService {
  WaveformService({required LocalStorageService storage}) : _storage = storage;

  final LocalStorageService _storage;

  /// Analyzes [audioFilePath]. Returns up to [targetBuckets] peaks in [0, 1]
  /// and the audio duration in milliseconds. Throws if decoding fails.
  Future<WaveformResult> analyze(
    String audioFilePath, {
    int targetBuckets = 1000,
  }) async {
    final cacheDir = await _storage.assetDir('waveform_cache');
    // The .wave is a throwaway intermediate (the real cache is the content-hash
    // DB table): give it a per-call unique name so concurrent extractions can't
    // collide on a path-derived name, and delete it in the finally so it doesn't
    // accumulate on disk forever.
    final waveOut = File(p.join(cacheDir.path, '${newId()}.wave'));

    try {
      Waveform? waveform;
      await for (final progress in JustWaveform.extract(
        audioInFile: File(audioFilePath),
        waveOutFile: waveOut,
        zoom: const WaveformZoom.pixelsPerSecond(100),
      )) {
        if (progress.waveform != null) {
          waveform = progress.waveform;
        }
      }

      if (waveform == null) {
        // just_waveform completes with a null waveform on an undecodable or
        // too-short file. Fail LOUDLY so callers (all inside _runBusy) abort and
        // toast, instead of silently persisting a 0-length, waveform-less clip
        // that renders as an invisible lane and is treated as valid data.
        throw const FormatException(
          "Couldn't read this audio — it may be corrupt, too short, or an "
          'unsupported format.',
        );
      }

      return (
        peaks: _downsample(waveform, targetBuckets),
        durationMs: waveform.duration.inMilliseconds,
      );
    } finally {
      if (await waveOut.exists()) {
        try {
          await waveOut.delete();
        } catch (_) {}
      }
    }
  }

  /// Collapses the per-pixel min/max samples into [target] normalized peaks,
  /// taking the largest absolute amplitude in each bucket.
  List<double> _downsample(Waveform waveform, int target) {
    final pixels = waveform.length;
    if (pixels <= 0) return const [];

    final buckets = target < pixels ? target : pixels;
    final peaks = List<double>.filled(buckets, 0);

    for (var b = 0; b < buckets; b++) {
      final start = (b * pixels / buckets).floor();
      final end = (((b + 1) * pixels / buckets).floor()).clamp(
        start + 1,
        pixels,
      );
      var peak = 0.0;
      for (var i = start; i < end; i++) {
        final min = waveform.getPixelMin(i).abs();
        final max = waveform.getPixelMax(i).abs();
        final amplitude = (min > max ? min : max) / 32768.0;
        if (amplitude > peak) peak = amplitude;
      }
      peaks[b] = peak > 1.0 ? 1.0 : peak;
    }
    return peaks;
  }
}
