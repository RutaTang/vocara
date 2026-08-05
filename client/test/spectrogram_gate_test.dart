import 'dart:io';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/spectrogram_service.dart';

/// Builds a RIFF/WAVE header with an arbitrary chunk sequence and a `data`
/// chunk whose DECLARED size is [declaredDataBytes] — the payload itself is
/// not written (the gate must judge the header, never load the body).
Uint8List _wavHeader({
  required int sampleRate,
  required int blockAlign,
  required int declaredDataBytes,
  bool extensible = false,
  bool extraListChunk = false,
}) {
  final b = BytesBuilder();
  void str(String s) => b.add(s.codeUnits);
  void u32(int v) =>
      b.add((ByteData(4)..setUint32(0, v, Endian.little)).buffer.asUint8List());
  void u16(int v) =>
      b.add((ByteData(2)..setUint16(0, v, Endian.little)).buffer.asUint8List());
  str('RIFF');
  u32(0xFFFFFFF0); // declared RIFF size is irrelevant to the gate
  str('WAVE');
  if (extraListChunk) {
    str('LIST');
    u32(12);
    b.add(Uint8List(12)); // metadata the walk must seek over
  }
  final fmtSize = extensible ? 40 : 16;
  str('fmt ');
  u32(fmtSize);
  u16(extensible ? 0xFFFE : 1);
  u16(1); // channels
  u32(sampleRate);
  u32(sampleRate * blockAlign);
  u16(blockAlign);
  u16(16);
  if (extensible) {
    u16(22); // cbSize
    u16(16); // valid bits
    u32(0x4); // channel mask
    // PCM SubFormat GUID
    b.add(const [
      0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, //
      0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71, //
    ]);
  }
  str('data');
  u32(declaredDataBytes);
  return b.toBytes();
}

void main() {
  // 30 min cap @ 44.1 kHz mono 16-bit = 44100 * 2 bytes * 1800 s.
  const underCap = 44100 * 2 * 60; // 1 minute
  const overCap = 44100 * 2 * 1860; // 31 minutes

  Future<File> write(Uint8List bytes) async {
    final dir = await Directory.systemTemp.createTemp('vocara_gate');
    addTearDown(() => dir.delete(recursive: true));
    final f = File('${dir.path}/probe.wav');
    await f.writeAsBytes(bytes);
    return f;
  }

  test('a canonical WAV under the cap is admitted', () async {
    final f = await write(
      _wavHeader(sampleRate: 44100, blockAlign: 2, declaredDataBytes: underCap),
    );
    expect(await SpectrogramService.shouldSkipAnalysis(f), isFalse);
  });

  test('an EXTENSIBLE WAV with extra chunks is still measured, and skipped '
      'over the cap', () async {
    // The old canonical-44-byte probe returned "unknown" for exactly this
    // shape and the whole file was then loaded; the chunk walk must both admit
    // a short one and refuse a long one.
    final short = await write(
      _wavHeader(
        sampleRate: 44100,
        blockAlign: 2,
        declaredDataBytes: underCap,
        extensible: true,
        extraListChunk: true,
      ),
    );
    expect(await SpectrogramService.shouldSkipAnalysis(short), isFalse);

    final long = await write(
      _wavHeader(
        sampleRate: 44100,
        blockAlign: 2,
        declaredDataBytes: overCap,
        extensible: true,
        extraListChunk: true,
      ),
    );
    expect(await SpectrogramService.shouldSkipAnalysis(long), isTrue);
  });

  test(
    'a canonical WAV over the cap is skipped from the header alone',
    () async {
      // The file holds only the header: judging by size-on-disk would admit it;
      // the gate must trust the declared data length instead.
      final f = await write(
        _wavHeader(
          sampleRate: 44100,
          blockAlign: 2,
          declaredDataBytes: overCap,
        ),
      );
      expect(await SpectrogramService.shouldSkipAnalysis(f), isTrue);
    },
  );

  test('non-WAV files are skipped without reading the body', () async {
    final f = await write(Uint8List.fromList(List.filled(4096, 7)));
    expect(await SpectrogramService.shouldSkipAnalysis(f), isTrue);
  });

  test('a missing file is skipped, not crashed on', () async {
    expect(
      await SpectrogramService.shouldSkipAnalysis(File('/nonexistent/x.wav')),
      isTrue,
    );
  });
}
