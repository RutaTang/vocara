import 'dart:io';
import 'dart:math' as math;
import 'dart:typed_data';

import 'package:flutter/foundation.dart' show visibleForTesting;
import 'package:just_waveform/just_waveform.dart';
import 'package:path/path.dart' as p;

import '../../utils/id.dart';
import 'audio_file_service.dart';
import 'local_storage_service.dart';

/// Result of analyzing an audio file: normalized peaks plus its duration.
typedef WaveformResult = ({List<double> peaks, int durationMs});

/// Extracts a renderable waveform from an audio file using just_waveform
/// (AVFoundation on macOS), then down-samples it to a compact, normalized peak
/// list that is stored on the track and drawn by a CustomPainter.
class WaveformService {
  WaveformService({required LocalStorageService storage}) : _storage = storage;

  final LocalStorageService _storage;

  /// The app-support folder [analyze] writes its throwaway `.wave`
  /// intermediates to.
  static const scratchDirName = 'waveform_cache';

  /// Deletes the `.wave` intermediates left under [root]'s [scratchDirName]
  /// by analyses that never reached their own cleanup: the app quit or was
  /// killed mid-analysis (a quit's budget cuts a stop's finalize off right
  /// there). They are derived scratch, never the only copy of anything, but
  /// nothing else ever looked in the folder again. Boot-only, AFTER the boot's
  /// own analyses (staged-recording recovery) and before any screen can start
  /// one: nothing may be in flight. Best-effort; never throws. Returns how
  /// many it removed.
  static Future<int> sweepScratch(Directory root) async {
    var removed = 0;
    try {
      final dir = Directory(p.join(root.path, scratchDirName));
      if (!await dir.exists()) return 0;
      await for (final e in dir.list()) {
        if (e is! File || p.extension(e.path) != '.wave') continue;
        try {
          await e.delete();
          removed++;
        } catch (_) {}
      }
    } catch (_) {}
    return removed;
  }

  /// Analyzes [audioFilePath]. Returns up to [targetBuckets] peaks in [0, 1]
  /// and the audio duration in milliseconds. Throws if decoding fails.
  Future<WaveformResult> analyze(
    String audioFilePath, {
    int targetBuckets = 1000,
  }) async {
    final cacheDir = await _storage.assetDir(scratchDirName);
    // The .wave is a throwaway intermediate (the real cache is the content-hash
    // DB table): give it a per-call unique name so concurrent extractions can't
    // collide on a path-derived name, and delete it in the finally so it doesn't
    // accumulate on disk forever (what a quit or kill leaves before the finally
    // runs is cleared at the next boot, see [sweepScratch]).
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
        peaks: downsample(waveform, targetBuckets),
        durationMs: await exactDurationMs(
          audioFilePath,
          waveform.duration.inMilliseconds,
        ),
      );
    } finally {
      if (await waveOut.exists()) {
        try {
          await waveOut.delete();
        } catch (_) {}
      }
    }
  }

  /// [path]'s length to the millisecond when it is a WAV (every clip is, once
  /// imported), else [waveformMs]. The waveform counts whole 10 ms pixels, up
  /// to 10 ms short of the audio: a merge shifted the bottom take's notes
  /// that much early, a split left a gap over audio that plays, and a loop
  /// over the whole song dropped the tail. The header's count is used only
  /// where it agrees with the decoded one to within that grid.
  static Future<int> exactDurationMs(String path, int waveformMs) async {
    try {
      final wav = await AudioFileService.probeWav(File(path));
      if (wav == null) return waveformMs;
      final frameBytes = wav.channels * wav.bitsPerSample ~/ 8;
      final ms = (wav.dataBytes ~/ frameBytes * 1000 / wav.sampleRate).round();
      return ms >= waveformMs && ms - waveformMs <= 20 ? ms : waveformMs;
    } catch (_) {
      return waveformMs;
    }
  }

  /// The `.wave` header: version, flags, sample rate, samples per pixel and
  /// pixel count, four bytes each; the (min, max) pixel pairs follow.
  static const _waveHeaderBytes = 20;

  /// The (min, max) pairs of [waveform], read where the file holds them.
  /// just_waveform 0.0.7 views them at byte 10, not 20
  /// (`Int16List.view(bytes, headerLength ~/ 2)` takes a BYTE offset), so its
  /// getPixelMin/Max read the header's last ten bytes as the first pixels and
  /// every pixel after them two and a half pixels late: each take began with
  /// a spike (the low half of its pixel count: over half full scale past three
  /// minutes) and was drawn 25 ms late. Read at byte 20 whatever the package
  /// does, so a fixed version reads the same.
  static Int16List _pixelPairs(Waveform waveform) {
    final data = waveform.data;
    if (data is! Int16List) return Int16List(0); // the darwin writer is 16-bit
    final buffer = data.buffer;
    final held = math.max(0, (buffer.lengthInBytes - _waveHeaderBytes) ~/ 4);
    final pixels = math.min(waveform.length, held);
    return Int16List.view(buffer, _waveHeaderBytes, pixels * 2);
  }

  /// Collapses the per-pixel min/max samples into [target] normalized peaks,
  /// taking the largest absolute amplitude in each bucket.
  @visibleForTesting
  static List<double> downsample(Waveform waveform, int target) {
    final pairs = _pixelPairs(waveform);
    final pixels = pairs.length ~/ 2;
    if (pixels <= 0) return const [];

    final buckets = target < pixels ? target : pixels;
    final peaks = List<double>.filled(buckets, 0);

    for (var b = 0; b < buckets; b++) {
      final start = (b * pixels / buckets).floor();
      final end = ((b + 1) * pixels / buckets).floor().clamp(start + 1, pixels);
      var peak = 0.0;
      for (var i = start; i < end; i++) {
        final min = pairs[2 * i].abs();
        final max = pairs[2 * i + 1].abs();
        final amplitude = (min > max ? min : max) / 32768.0;
        if (amplitude > peak) peak = amplitude;
      }
      peaks[b] = peak > 1.0 ? 1.0 : peak;
    }
    return peaks;
  }
}
