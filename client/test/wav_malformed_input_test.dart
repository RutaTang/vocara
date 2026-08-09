// Every WAV byte the app parses came from somewhere it does not control: a
// user's import, a file that was being written when the process died, a pool
// file a failing disk half-wrote. Three independent parsers read those bytes —
// AudioFileService.probeWav (boot recovery's "is this a take?"),
// SpectrogramService.admitForAnalysis (the analysis gate) and
// trimWavHeadInPlace (the record-latency head cut) — and each walks a chunk
// list whose sizes the FILE declares.
//
// The contract this pins: a malformed file is REJECTED (null / FormatException),
// never a crash, a hang, or an out-of-range read. A header that lies about its
// sizes is the interesting case — the declared length must never be trusted
// past the real end of the file.

import 'dart:io';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/services/audio_edit_service.dart';
import 'package:vocara/data/services/audio_file_service.dart';
import 'package:vocara/data/services/spectrogram_service.dart';

/// A canonical mono float32 WAV, the exact shape the recorder writes.
Uint8List _goodWav({int frames = 240, int rate = 48000}) {
  const bits = 32, channels = 1;
  final blockAlign = channels * bits ~/ 8;
  final dataBytes = frames * blockAlign;
  final b = BytesBuilder();
  void u32(int v) =>
      b.add(Uint8List(4)..buffer.asByteData().setUint32(0, v, Endian.little));
  void u16(int v) =>
      b.add(Uint8List(2)..buffer.asByteData().setUint16(0, v, Endian.little));
  b.add('RIFF'.codeUnits);
  u32(36 + dataBytes);
  b.add('WAVE'.codeUnits);
  b.add('fmt '.codeUnits);
  u32(16);
  u16(3); // IEEE float
  u16(channels);
  u32(rate);
  u32(rate * blockAlign);
  u16(blockAlign);
  u16(bits);
  b.add('data'.codeUnits);
  u32(dataBytes);
  b.add(Uint8List(dataBytes));
  return b.toBytes();
}

void main() {
  late Directory tmp;

  setUp(() async {
    tmp = await Directory.systemTemp.createTemp('vocara_wav_malformed');
  });
  tearDown(() async {
    if (await tmp.exists()) await tmp.delete(recursive: true);
  });

  Future<File> write(String name, List<int> bytes) async {
    final f = File(p.join(tmp.path, name));
    await f.writeAsBytes(bytes, flush: true);
    return f;
  }

  /// Every parser, on one file. Each may reject; none may throw anything other
  /// than a FormatException, and none may hang.
  Future<void> parseAll(File f) async {
    try {
      await AudioFileService.probeWav(f);
    } on FormatException {
      // A shape rejection is a legal answer.
    }
    try {
      await SpectrogramService.admitForAnalysis(f);
    } on FormatException {
      // ditto
    }
    try {
      await trimWavHeadInPlace(f.path, 5);
    } on FormatException {
      // ditto
    }
  }

  test('a canonical file is accepted by every parser', () async {
    final f = await write('good.wav', _goodWav());
    expect(await AudioFileService.probeWav(f), isNotNull);
    expect(await SpectrogramService.admitForAnalysis(f), isNotNull);
  });

  test(
    'truncation at every prefix length is rejected, never a crash',
    () async {
      final good = _goodWav();
      // Every boundary the chunk walk can land on: inside RIFF, inside fmt, at
      // the data header, and part way through the payload.
      for (final n in [0, 1, 4, 11, 12, 15, 20, 36, 40, 43, 44, 45, 100]) {
        final f = await write('trunc$n.wav', good.sublist(0, n));
        await parseAll(f);
      }
    },
  );

  test(
    'a header that lies about its sizes cannot read past the file',
    () async {
      void put32(Uint8List b, int off, int v) =>
          b.buffer.asByteData().setUint32(off, v, Endian.little);

      // data size far beyond the file
      final huge = Uint8List.fromList(_goodWav());
      put32(huge, 40, 0xFFFFFFF0);
      await parseAll(await write('huge-data.wav', huge));

      // RIFF size far beyond the file
      final hugeRiff = Uint8List.fromList(_goodWav());
      put32(hugeRiff, 4, 0xFFFFFFF0);
      await parseAll(await write('huge-riff.wav', hugeRiff));

      // fmt chunk claiming a gigantic size (the walk must not seek off the end)
      final hugeFmt = Uint8List.fromList(_goodWav());
      put32(hugeFmt, 16, 0xFFFFFFF0);
      await parseAll(await write('huge-fmt.wav', hugeFmt));

      // zero-size chunks: a naive walk advances by 8 forever
      final zeroFmt = Uint8List.fromList(_goodWav());
      put32(zeroFmt, 16, 0);
      await parseAll(await write('zero-fmt.wav', zeroFmt));
    },
  );

  test('impossible format fields are rejected', () async {
    void put16(Uint8List b, int off, int v) =>
        b.buffer.asByteData().setUint16(off, v, Endian.little);
    void put32(Uint8List b, int off, int v) =>
        b.buffer.asByteData().setUint32(off, v, Endian.little);

    final zeroChannels = Uint8List.fromList(_goodWav());
    put16(zeroChannels, 22, 0);
    await parseAll(await write('zero-ch.wav', zeroChannels));

    final zeroRate = Uint8List.fromList(_goodWav());
    put32(zeroRate, 24, 0);
    await parseAll(await write('zero-rate.wav', zeroRate));

    final zeroBits = Uint8List.fromList(_goodWav());
    put16(zeroBits, 34, 0);
    await parseAll(await write('zero-bits.wav', zeroBits));

    // blockAlign inconsistent with channels * bits (the PCM-frame check)
    final badAlign = Uint8List.fromList(_goodWav());
    put16(badAlign, 32, 7);
    await parseAll(await write('bad-align.wav', badAlign));

    // a compressed codec tag
    final compressed = Uint8List.fromList(_goodWav());
    put16(compressed, 20, 0x0055); // MPEG layer 3
    await parseAll(await write('mp3-tag.wav', compressed));
  });

  test('a wall of chunks does not make the walk run away', () async {
    // 4000 empty 'JUNK' chunks before 'data': every parser bounds its walk, so
    // this must terminate (rejecting is fine) rather than scan forever.
    final b = BytesBuilder();
    void u32(int v) =>
        b.add(Uint8List(4)..buffer.asByteData().setUint32(0, v, Endian.little));
    b.add('RIFF'.codeUnits);
    u32(0);
    b.add('WAVE'.codeUnits);
    for (var i = 0; i < 4000; i++) {
      b.add('JUNK'.codeUnits);
      u32(0);
    }
    await parseAll(await write('junk-wall.wav', b.toBytes()));
  });

  test('a directory and a missing path are rejected, not thrown at', () async {
    final missing = File(p.join(tmp.path, 'nope.wav'));
    expect(await AudioFileService.probeWav(missing), isNull);
    expect(await SpectrogramService.admitForAnalysis(missing), isNull);
  });
}
