import 'dart:io';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/audio_dsp.dart';
import 'package:vocara/data/services/audio_edit_service.dart';
import 'package:vocara/data/services/audio_file_service.dart';
import 'package:vocara/data/services/local_storage_service.dart';

/// An [AudioFileService] that hands out temp-dir paths so the edit service can
/// be exercised without the macOS sandbox / path_provider.
class _TempFiles extends AudioFileService {
  _TempFiles(this.dir) : super(storage: LocalStorageService());

  final Directory dir;
  int _n = 0;

  @override
  Future<String> newRecordingPath({
    required String practiceId,
    String extension = 'wav',
  }) async =>
      '${dir.path}/clip_${_n++}.$extension';
}

/// Builds a mono 16-bit WAV tagged WAVE_FORMAT_EXTENSIBLE (0xFFFE), the way
/// AVFoundation often writes recordings.
Uint8List _extensibleWavPcm16(Int16List samples, int sr) {
  final data = Uint8List(samples.length * 2);
  final dv = ByteData.sublistView(data);
  for (var i = 0; i < samples.length; i++) {
    dv.setInt16(i * 2, samples[i], Endian.little);
  }
  final b = BytesBuilder();
  void str(String s) => b.add(s.codeUnits);
  void u32(int v) =>
      b.add((ByteData(4)..setUint32(0, v, Endian.little)).buffer.asUint8List());
  void u16(int v) =>
      b.add((ByteData(2)..setUint16(0, v, Endian.little)).buffer.asUint8List());
  const fmtSize = 40;
  str('RIFF');
  u32(4 + (8 + fmtSize) + (8 + data.length));
  str('WAVE');
  str('fmt ');
  u32(fmtSize);
  u16(0xFFFE); // WAVE_FORMAT_EXTENSIBLE
  u16(1); // channels
  u32(sr); // sample rate
  u32(sr * 2); // byte rate
  u16(2); // block align
  u16(16); // bits per sample
  u16(22); // cbSize
  u16(16); // valid bits
  u32(0); // channel mask
  u16(1); // SubFormat tag = PCM
  b.add(const [
    0x00, 0x00, 0x00, 0x00, 0x10, 0x00, //
    0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71, //
  ]);
  str('data');
  u32(data.length);
  b.add(data);
  return b.toBytes();
}

void main() {
  test('splitWav cuts a take into two parts that rejoin to the original',
      () async {
    final dir = await Directory.systemTemp.createTemp('vocara_edit');
    addTearDown(() => dir.delete(recursive: true));

    const sr = 8000;
    // A 1s ramp from -1..1 so the two halves are clearly distinguishable and a
    // continuous cut is easy to assert.
    final samples = Float64List.fromList([
      for (var i = 0; i < sr; i++) (i / sr) * 2 - 1,
    ]);
    final srcPath = '${dir.path}/src.wav';
    await File(srcPath).writeAsBytes(writeWavMono(samples, sr));

    final edit = AudioEditService(files: _TempFiles(dir));
    final parts = await edit.splitWav(
      practiceId: 'p',
      sourcePath: srcPath,
      splitFileMs: 400,
    );

    final first = readWavMono(await File(parts.firstPath).readAsBytes())!;
    final second = readWavMono(await File(parts.secondPath).readAsBytes())!;
    expect(first.sampleRate, sr);
    // 400 ms at 8 kHz = 3200 samples in the first part.
    expect(first.samples.length, 3200);
    expect(second.samples.length, sr - 3200);
    // The cut is seamless: the two slices bracket the original ramp.
    expect(first.samples.last, closeTo(samples[3199], 2e-4));
    expect(second.samples.first, closeTo(samples[3200], 2e-4));

    final mergedPath = await edit.concatWav(
      practiceId: 'p',
      firstPath: parts.firstPath,
      secondPath: parts.secondPath,
    );
    final round = readWavMono(await File(mergedPath).readAsBytes())!;
    expect(round.samples.length, sr);
    for (var i = 0; i < sr; i += 137) {
      expect(round.samples[i], closeTo(samples[i], 2e-4));
    }
  });

  test('reads and splits a WAVE_FORMAT_EXTENSIBLE recording', () async {
    // AVFoundation (the `record` plugin's macOS backend) can write WAVs tagged
    // 0xFFFE rather than plain PCM. Both the decoder and the byte-level split
    // must handle it.
    final dir = await Directory.systemTemp.createTemp('vocara_ext');
    addTearDown(() => dir.delete(recursive: true));

    const sr = 8000;
    final samples =
        Int16List.fromList([for (var i = 0; i < sr; i++) (i % 200) * 100 - 10000]);
    final srcPath = '${dir.path}/rec.wav';
    await File(srcPath).writeAsBytes(_extensibleWavPcm16(samples, sr));

    final pcm = readWavMono(await File(srcPath).readAsBytes());
    expect(pcm, isNotNull, reason: 'readWavMono must accept EXTENSIBLE PCM16');
    expect(pcm!.samples.length, sr);

    final edit = AudioEditService(files: _TempFiles(dir));
    final parts = await edit.splitWav(
      practiceId: 'p',
      sourcePath: srcPath,
      splitFileMs: 250,
    );
    final first = readWavMono(await File(parts.firstPath).readAsBytes())!;
    final second = readWavMono(await File(parts.secondPath).readAsBytes())!;
    expect(first.samples.length, 2000); // 250 ms at 8 kHz
    expect(second.samples.length, sr - 2000);
  });

  test('concatWav rejects takes with mismatched formats instead of corrupting '
      'the second half', () async {
    final dir = await Directory.systemTemp.createTemp('vocara_concat');
    addTearDown(() => dir.delete(recursive: true));

    final tone =
        Float64List.fromList([for (var i = 0; i < 400; i++) (i % 50) / 50 - 0.5]);
    final a = '${dir.path}/a.wav';
    final b = '${dir.path}/b.wav';
    // Identical content, different sample rate -> different fmt header. Joining
    // them under a's header would replay b's half at the wrong rate.
    await File(a).writeAsBytes(writeWavMono(tone, 44100));
    await File(b).writeAsBytes(writeWavMono(tone, 48000));

    final edit = AudioEditService(files: _TempFiles(dir));
    expect(
      () => edit.concatWav(practiceId: 'p', firstPath: a, secondPath: b),
      throwsA(isA<FormatException>()),
    );

    // Same format still joins fine.
    final c = '${dir.path}/c.wav';
    await File(c).writeAsBytes(writeWavMono(tone, 44100));
    final out =
        await edit.concatWav(practiceId: 'p', firstPath: a, secondPath: c);
    expect(readWavMono(await File(out).readAsBytes())!.samples.length, 800);
  });

  test('concatWav merges a plain-PCM take with an EXTENSIBLE take of the same '
      'actual format (rate/channels/bits)', () async {
    final dir = await Directory.systemTemp.createTemp('vocara_concat_ext');
    addTearDown(() => dir.delete(recursive: true));
    const sr = 8000;
    final pcm = '${dir.path}/pcm.wav'; // 16-byte PCM fmt
    final ext = '${dir.path}/ext.wav'; // 40-byte EXTENSIBLE fmt
    await File(pcm).writeAsBytes(
        writeWavMono(Float64List.fromList([for (var i = 0; i < 100; i++) 0.1]), sr));
    await File(ext).writeAsBytes(_extensibleWavPcm16(
        Int16List.fromList([for (var i = 0; i < 100; i++) 1000]), sr));

    final edit = AudioEditService(files: _TempFiles(dir));
    final out =
        await edit.concatWav(practiceId: 'p', firstPath: pcm, secondPath: ext);
    expect(readWavMono(await File(out).readAsBytes())!.samples.length, 200);
  });

  test('splitWav rejects non-WAV audio', () async {
    final dir = await Directory.systemTemp.createTemp('vocara_edit');
    addTearDown(() => dir.delete(recursive: true));
    final bogus = '${dir.path}/song.m4a';
    await File(bogus).writeAsBytes(Uint8List.fromList(List.filled(2048, 7)));

    final edit = AudioEditService(files: _TempFiles(dir));
    expect(
      () => edit.splitWav(practiceId: 'p', sourcePath: bogus, splitFileMs: 100),
      throwsA(isA<FormatException>()),
    );
  });
}
