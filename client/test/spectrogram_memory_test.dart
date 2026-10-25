import 'dart:async';
import 'dart:io';
import 'dart:math' as math;
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/spectrogram_service.dart';

/// Writes a mono 32-bit float WAV of [secs] seconds (the take format), one
/// second at a time so the fixture itself never holds the track.
Future<File> _floatWav(String path, int sr, int secs) async {
  final frames = sr * secs;
  final header = ByteData(44);
  void tag(int o, String s) {
    for (var i = 0; i < 4; i++) {
      header.setUint8(o + i, s.codeUnitAt(i));
    }
  }

  tag(0, 'RIFF');
  header.setUint32(4, 36 + frames * 4, Endian.little);
  tag(8, 'WAVE');
  tag(12, 'fmt ');
  header.setUint32(16, 16, Endian.little);
  header.setUint16(20, 3, Endian.little); // IEEE float
  header.setUint16(22, 1, Endian.little);
  header.setUint32(24, sr, Endian.little);
  header.setUint32(28, sr * 4, Endian.little);
  header.setUint16(32, 4, Endian.little);
  header.setUint16(34, 32, Endian.little);
  tag(36, 'data');
  header.setUint32(40, frames * 4, Endian.little);
  final raf = await File(path).open(mode: FileMode.write);
  await raf.writeFrom(header.buffer.asUint8List());
  final block = Float32List(sr);
  for (var s = 0; s < secs; s++) {
    for (var i = 0; i < sr; i++) {
      block[i] = 0.4 * math.sin(2 * math.pi * 220 * (s * sr + i) / sr);
    }
    await raf.writeFrom(block.buffer.asUint8List());
  }
  await raf.close();
  return File(path);
}

void main() {
  test(
    'a long wide-range spectrogram peaks near its own size, not twice it',
    () async {
      // The 18 kHz EQ display spectrogram of a long take is the largest one
      // the app builds (769 bins a frame). Every column used to be its own
      // list until the end, which then copied them all into the flat image:
      // the compute held the whole result twice. Its frame count is known
      // from the header, so it now fills one buffer of exactly that size.
      // Measured on this 10-minute take (a 173 MB image): ~380 MB of growth
      // before, ~250 MB after (the rest is the compute's transient garbage).
      final dir = await Directory.systemTemp.createTemp('vocara_spec_mem');
      addTearDown(() => dir.delete(recursive: true));
      const sr = 48000, secs = 600;
      final wav = await _floatWav('${dir.path}/take.wav', sr, secs);

      final baseline = ProcessInfo.currentRss;
      var peak = baseline;
      final sampler = Timer.periodic(const Duration(milliseconds: 2), (_) {
        final rss = ProcessInfo.currentRss;
        if (rss > peak) peak = rss;
      });
      final spec = await const SpectrogramService().computeFromFile(
        wav.path,
        maxHz: 18000,
      );
      sampler.cancel();

      expect(spec, isNotNull);
      final resultBytes = spec!.db.lengthInBytes;
      final grew = peak - baseline;
      expect(
        grew,
        lessThan(resultBytes * 1.75),
        reason:
            'peak growth ${grew ~/ 1000000} MB for a '
            '${resultBytes ~/ 1000000} MB spectrogram',
      );
    },
    timeout: const Timeout(Duration(minutes: 3)),
  );
}
