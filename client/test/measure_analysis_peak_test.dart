import 'dart:io';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/spectrogram_service.dart';

/// MANUAL measurement, not a CI assertion: run with
///   VOCARA_MEASURE=1 flutter test test/measure_analysis_peak_test.dart
/// It generates the largest legal analysis WAV (10 min, 48 kHz, mono f32),
/// runs computeBothFromFile, and reports the process RSS delta.
///
/// SCOPE: this measures the WAV path only — SpectrogramService's windowed
/// StreamingTakeAnalyzer, which never materializes the track. It is NOT the
/// arbitrary-media path (MediaAnalysisService), which decodes through the
/// native engine and holds a full-track buffer on its own side; a VM test
/// cannot load the engine framework, so that path stays a derivation rather
/// than a measurement. Quoting this number for it would understate it by the
/// whole track.
void main() {
  test(
    'measure computeBothFromFile peak RSS (manual)',
    () async {
      if (!Platform.environment.containsKey('VOCARA_MEASURE')) {
        return; // enabled explicitly; a measurement is not a pass/fail check
      }
      final dir = await Directory.systemTemp.createTemp('vocara_measure');
      addTearDown(() => dir.delete(recursive: true));
      const sr = 48000;
      const secs = 600; // the 10-minute analysis cap
      final f = File('${dir.path}/max.wav');
      final sink = f.openSync(mode: FileMode.write);
      final header = BytesBuilder();
      void str(String s) => header.add(s.codeUnits);
      void u32(int v) => header.add(
        (ByteData(4)..setUint32(0, v, Endian.little)).buffer.asUint8List(),
      );
      void u16(int v) => header.add(
        (ByteData(2)..setUint16(0, v, Endian.little)).buffer.asUint8List(),
      );
      const frames = sr * secs;
      const dataLen = frames * 4;
      str('RIFF');
      u32(36 + dataLen);
      str('WAVE');
      str('fmt ');
      u32(16);
      u16(3); // IEEE float
      u16(1);
      u32(sr);
      u32(sr * 4);
      u16(4);
      u16(32);
      str('data');
      u32(dataLen);
      sink.writeFromSync(header.toBytes());
      // A 220 Hz tone chunked out in 1 s blocks (silence would let clever paths
      // shortcut; a tone exercises the full DSP).
      final block = Float32List(sr);
      for (var i = 0; i < sr; i++) {
        block[i] = 0.5 * (i * 220 * 2 * 3.141592653589793 / sr).remainder(6.28);
      }
      final blockBytes = block.buffer.asUint8List();
      for (var s = 0; s < secs; s++) {
        sink.writeFromSync(blockBytes);
      }
      sink.closeSync();

      final baseline = ProcessInfo.currentRss;
      var peak = baseline;
      final sampler = Stream<void>.periodic(const Duration(milliseconds: 50))
          .listen((_) {
            final rss = ProcessInfo.currentRss;
            if (rss > peak) peak = rss;
          });
      const service = SpectrogramService();
      final result = await service.computeBothFromFile(f.path);
      await sampler.cancel();
      expect(result?.spectrogram, isNotNull);
      expect(result?.metrics, isNotNull);
      final deltaMb = (peak - baseline) / (1000 * 1000);
      // ignore: avoid_print — the whole point of this manual run is the number.
      print(
        'computeBothFromFile 10min@48k f32: RSS delta ≈ '
        '${deltaMb.toStringAsFixed(0)} MB (baseline '
        '${(baseline / 1000000).toStringAsFixed(0)} MB, peak '
        '${(peak / 1000000).toStringAsFixed(0)} MB)',
      );
    },
    timeout: const Timeout(Duration(minutes: 5)),
  );
}
