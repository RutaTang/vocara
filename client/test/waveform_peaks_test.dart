// just_waveform 0.0.7 views a .wave file's pixels at byte 10 instead of 20, so
// its pixel getters read header fields as the first pixels and every pixel
// after them late. The service reads the pixels itself.

import 'dart:io';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:just_waveform/just_waveform.dart';
import 'package:vocara/data/services/waveform_service.dart';

/// A .wave file as the package's darwin writer lays it out: a 20-byte header
/// (version, flags, sample rate, samples per pixel, pixel count) and 16-bit
/// (min, max) pairs.
Uint8List _wave(List<int> peakPerPixel) {
  final b = ByteData(20 + peakPerPixel.length * 4);
  final header = [1, 0, 44100, 441, peakPerPixel.length];
  for (var i = 0; i < 5; i++) {
    b.setUint32(i * 4, header[i], Endian.host);
  }
  for (var i = 0; i < peakPerPixel.length; i++) {
    b.setInt16(20 + i * 4, -peakPerPixel[i], Endian.host);
    b.setInt16(22 + i * 4, peakPerPixel[i], Endian.host);
  }
  return b.buffer.asUint8List();
}

void main() {
  test('peaks start at the first pixel: no header spike, no lag', () async {
    final dir = await Directory.systemTemp.createTemp('wave');
    addTearDown(() => dir.delete(recursive: true));
    // Three minutes of silence at 100 px/s with one loud pixel at 50.
    final pixels = List.filled(18000, 0)..[50] = 16384;
    final file = File('${dir.path}/take.wave')..writeAsBytesSync(_wave(pixels));

    final peaks = WaveformService.downsample(
      await JustWaveform.parse(file),
      pixels.length,
    );

    expect(peaks, hasLength(18000));
    expect(peaks.take(3), everyElement(0.0), reason: 'no header spike');
    expect(peaks[50], 0.5);
    expect(peaks[51], 0.0);
    expect(peaks[52], 0.0, reason: 'not drawn late');
  });

  test(
    'a file shorter than its header says reads only what it holds',
    () async {
      final dir = await Directory.systemTemp.createTemp('wave');
      addTearDown(() => dir.delete(recursive: true));
      final bytes = _wave(List.filled(100, 3276));
      // Claim more pixels than the file holds.
      ByteData.sublistView(bytes).setUint32(16, 500, Endian.host);
      final file = File('${dir.path}/take.wave')..writeAsBytesSync(bytes);

      final peaks = WaveformService.downsample(
        await JustWaveform.parse(file),
        1000,
      );

      expect(peaks, hasLength(100));
      expect(peaks, everyElement(closeTo(0.1, 1e-4)));
    },
  );
}
