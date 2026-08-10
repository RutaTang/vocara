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
  }) async => '${dir.path}/clip_${_n++}.$extension';
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

/// Builds a mono PCM16 WAV whose `fmt ` chunk declares an ODD size (17): the
/// 16 canonical bytes plus one extension byte, followed by the spec-required
/// word-alignment pad byte before `data`. The old import gate accepted ANY
/// uncompressed WAV, so a legacy library can still hold such files.
Uint8List _oddFmtWavPcm16(Int16List samples, int sr) {
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
  const fmtSize = 17;
  final dataPad = data.length.isOdd ? 1 : 0;
  str('RIFF');
  u32(4 + (8 + fmtSize + 1) + (8 + data.length + dataPad));
  str('WAVE');
  str('fmt ');
  u32(fmtSize);
  u16(1); // PCM
  u16(1); // channels
  u32(sr); // sample rate
  u32(sr * 2); // byte rate
  u16(2); // block align
  u16(16); // bits per sample
  b.addByte(0); // 17th fmt byte (a lone extension byte — size is what matters)
  b.addByte(0); // word-alignment pad before the next chunk
  str('data');
  u32(data.length);
  b.add(data);
  if (dataPad == 1) b.addByte(0);
  return b.toBytes();
}

/// Builds a canonical RIFF/WAVE file with an arbitrary 16-byte `fmt ` chunk
/// (tag/channels/rate/blockAlign/bits) and raw [data] — for exercising the
/// header validation with compressed or lying formats.
Uint8List _wavWithFmt({
  required int formatTag,
  required int channels,
  required int sampleRate,
  required int blockAlign,
  required int bits,
  required Uint8List data,
}) {
  final b = BytesBuilder();
  void str(String s) => b.add(s.codeUnits);
  void u32(int v) =>
      b.add((ByteData(4)..setUint32(0, v, Endian.little)).buffer.asUint8List());
  void u16(int v) =>
      b.add((ByteData(2)..setUint16(0, v, Endian.little)).buffer.asUint8List());
  str('RIFF');
  u32(36 + data.length);
  str('WAVE');
  str('fmt ');
  u32(16);
  u16(formatTag);
  u16(channels);
  u32(sampleRate);
  u32(sampleRate * blockAlign);
  u16(blockAlign);
  u16(bits);
  str('data');
  u32(data.length);
  b.add(data);
  return b.toBytes();
}

/// The `data` chunk payload of a canonical (RIFF + fmt + data) WAV.
Uint8List _dataChunk(Uint8List wav) {
  final dv = ByteData.sublistView(wav);
  var at = 12;
  while (at + 8 <= wav.length) {
    final id = String.fromCharCodes(wav, at, at + 4);
    final size = dv.getUint32(at + 4, Endian.little);
    if (id == 'data') return Uint8List.sublistView(wav, at + 8, at + 8 + size);
    at += 8 + size + (size.isOdd ? 1 : 0);
  }
  throw StateError('no data chunk');
}

void main() {
  // Regression guard: the split point was clamped against the RAW data length,
  // so a data chunk that isn't a whole number of frames could put the cut
  // INSIDE a frame — the second half then starts on the wrong channel and
  // plays with the stereo image swapped for its whole length.
  test('splitWav cuts on a frame boundary even when the data chunk is '
      'a partial frame long', () async {
    final dir = await Directory.systemTemp.createTemp('vocara_edit');
    addTearDown(() => dir.delete(recursive: true));

    const blockAlign = 4; // stereo, 16-bit
    const frames = 100;
    // 100 whole frames plus a stray half frame, as a write cut short leaves.
    final data = Uint8List(frames * blockAlign + 2);
    for (var i = 0; i < data.length; i++) {
      data[i] = i & 0xFF;
    }
    final srcPath = '${dir.path}/ragged.wav';
    await File(srcPath).writeAsBytes(
      _wavWithFmt(
        formatTag: 1,
        channels: 2,
        sampleRate: 8000,
        blockAlign: blockAlign,
        bits: 16,
        data: data,
      ),
    );

    final edit = AudioEditService(files: _TempFiles(dir));
    // Far past the end, so the cut clamps to the last legal frame boundary —
    // the case that used to land on the ragged tail.
    final parts = await edit.splitWav(
      practiceId: 'P0000000000000000000000000',
      sourcePath: srcPath,
      splitFileMs: 60000,
    );

    final first = _dataChunk(await File(parts.firstPath).readAsBytes());
    final second = _dataChunk(await File(parts.secondPath).readAsBytes());
    expect(
      first.length % blockAlign,
      0,
      reason: 'the cut must not split a frame',
    );
    expect(second.length % blockAlign, 0);
    expect(
      first.length + second.length,
      frames * blockAlign,
      reason: 'the trailing partial frame is dropped, never duplicated',
    );
    // Byte-for-byte continuous: the second half resumes exactly where the
    // first ended (a mid-frame cut would offset it by two bytes).
    expect(first, data.sublist(0, first.length));
    expect(second, data.sublist(first.length, first.length + second.length));
  });

  test(
    'splitWav cuts a take into two parts that rejoin to the original',
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
        practiceId: 'P0000000000000000000000000',
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
        practiceId: 'P0000000000000000000000000',
        firstPath: parts.firstPath,
        secondPath: parts.secondPath,
      );
      final round = readWavMono(await File(mergedPath).readAsBytes())!;
      expect(round.samples.length, sr);
      for (var i = 0; i < sr; i += 137) {
        expect(round.samples[i], closeTo(samples[i], 2e-4));
      }
    },
  );

  test('reads and splits a WAVE_FORMAT_EXTENSIBLE recording', () async {
    // AVFoundation (the `record` plugin's macOS backend) can write WAVs tagged
    // 0xFFFE rather than plain PCM. Both the decoder and the byte-level split
    // must handle it.
    final dir = await Directory.systemTemp.createTemp('vocara_ext');
    addTearDown(() => dir.delete(recursive: true));

    const sr = 8000;
    final samples = Int16List.fromList([
      for (var i = 0; i < sr; i++) (i % 200) * 100 - 10000,
    ]);
    final srcPath = '${dir.path}/rec.wav';
    await File(srcPath).writeAsBytes(_extensibleWavPcm16(samples, sr));

    final pcm = readWavMono(await File(srcPath).readAsBytes());
    expect(pcm, isNotNull, reason: 'readWavMono must accept EXTENSIBLE PCM16');
    expect(pcm!.samples.length, sr);

    final edit = AudioEditService(files: _TempFiles(dir));
    final parts = await edit.splitWav(
      practiceId: 'P0000000000000000000000000',
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

    final tone = Float64List.fromList([
      for (var i = 0; i < 400; i++) (i % 50) / 50 - 0.5,
    ]);
    final a = '${dir.path}/a.wav';
    final b = '${dir.path}/b.wav';
    // Identical content, different sample rate -> different fmt header. Joining
    // them under a's header would replay b's half at the wrong rate.
    await File(a).writeAsBytes(writeWavMono(tone, 44100));
    await File(b).writeAsBytes(writeWavMono(tone, 48000));

    final edit = AudioEditService(files: _TempFiles(dir));
    expect(
      () => edit.concatWav(
        practiceId: 'P0000000000000000000000000',
        firstPath: a,
        secondPath: b,
      ),
      throwsA(isA<FormatException>()),
    );

    // Same format still joins fine.
    final c = '${dir.path}/c.wav';
    await File(c).writeAsBytes(writeWavMono(tone, 44100));
    final out = await edit.concatWav(
      practiceId: 'P0000000000000000000000000',
      firstPath: a,
      secondPath: c,
    );
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
      writeWavMono(
        Float64List.fromList([for (var i = 0; i < 100; i++) 0.1]),
        sr,
      ),
    );
    await File(ext).writeAsBytes(
      _extensibleWavPcm16(
        Int16List.fromList([for (var i = 0; i < 100; i++) 1000]),
        sr,
      ),
    );

    final edit = AudioEditService(files: _TempFiles(dir));
    final out = await edit.concatWav(
      practiceId: 'P0000000000000000000000000',
      firstPath: pcm,
      secondPath: ext,
    );
    expect(readWavMono(await File(out).readAsBytes())!.samples.length, 200);
  });

  test('a validBitsPerSample or channel-mask mismatch is NOT byte-spliceable '
      '(24-in-32 style reinterpretation is corruption)', () {
    // The byte-splice keeps the FIRST header: joining a full-16 take with a
    // valid-bits-12 EXTENSIBLE take would reinterpret the second half's
    // samples under the wrong significance. Container bits match, so only
    // the validBits comparison can catch it — the incompatibility routes
    // concat through the decode -> re-encode path instead.
    const sr = 8000;
    final plain = writeWavMono(
      Float64List.fromList([for (var i = 0; i < 100; i++) 0.1]),
      sr,
    );
    final ext = _extensibleWavPcm16(
      Int16List.fromList([for (var i = 0; i < 100; i++) 1000]),
      sr,
    );
    expect(wavCompatibleForMerge(plain, ext), isTrue, reason: 'baseline');

    // Patch validBitsPerSample (fmt offset 18; fmt body starts at byte 20).
    final vb12 = Uint8List.fromList(ext)..[38] = 12;
    expect(wavCompatibleForMerge(plain, vb12), isFalse);

    // Two EXTENSIBLE takes with different channel masks must not byte-splice
    // under the first's mask either (fmt offset 20).
    final maskFront = Uint8List.fromList(ext)..[40] = 1;
    final maskBack = Uint8List.fromList(ext)..[40] = 4;
    expect(wavCompatibleForMerge(maskFront, maskBack), isFalse);
    expect(wavCompatibleForMerge(maskFront, maskFront), isTrue);

    // A plain fmt makes no speaker-position claim (mask 0), so it does not
    // match one that does — in EITHER order. The splice keeps the FIRST file's
    // header, so letting this through would stamp one half's channel layout
    // over the other's. The `ext` baseline above still passes because its mask
    // is 0 too.
    expect(wavCompatibleForMerge(plain, maskFront), isFalse);
    expect(wavCompatibleForMerge(maskFront, plain), isFalse);
  });

  test('ensureStreamableWav passes an ENGINE-RATE canonical WAV through '
      'untouched (no re-encode, no copy)', () async {
    final dir = await Directory.systemTemp.createTemp('vocara_streamable');
    addTearDown(() => dir.delete(recursive: true));
    final tone = Float64List.fromList([
      for (var i = 0; i < 400; i++) (i % 50) / 50 - 0.5,
    ]);
    final src = '${dir.path}/take.wav';
    await File(src).writeAsBytes(writeWavMono(tone, 48000));
    final before = await File(src).length();

    final edit = AudioEditService(files: _TempFiles(dir));
    final out = await edit.ensureStreamableWav(
      practiceId: 'P0000000000000000000000000',
      path: src,
    );
    expect(out, src, reason: 'a canonical engine-rate WAV is streamable');
    expect(await File(src).length(), before, reason: 'byte-identical');
    // The transcode branch itself needs the native engine (integration
    // coverage); the WAV passthrough is the decision this test pins.
  });

  test('isEngineStreamableWav mirrors the native feeder admission '
      '(WavTrackReader::open) — the two rules must not drift', () async {
    // Vectors matched by the engine's feed tests: only a canonical 44-byte
    // header at the engine rate with 1-2 PCM16/f32 channels streams from
    // disk; everything else must be transcoded at import or it silently
    // falls back to the full-decode RAM path.
    final dir = await Directory.systemTemp.createTemp('vocara_streamable_adm');
    addTearDown(() => dir.delete(recursive: true));
    final tone = Float64List.fromList([
      for (var i = 0; i < 64; i++) (i % 16) / 16 - 0.5,
    ]);
    Future<bool> judge(String name, List<int> bytes) async {
      final path = '${dir.path}/$name';
      await File(path).writeAsBytes(bytes);
      return AudioEditService.isEngineStreamableWav(path);
    }

    expect(await judge('ok48k.wav', writeWavMono(tone, 48000)), isTrue);
    expect(
      await judge('cd44k.wav', writeWavMono(tone, 44100)),
      isFalse,
      reason: '44.1 kHz is not the engine rate',
    );
    expect(
      await judge(
        'ext.wav',
        _extensibleWavPcm16(Int16List.fromList(List.filled(64, 100)), 48000),
      ),
      isFalse,
      reason: 'EXTENSIBLE fmt is not the canonical 44-byte layout',
    );
    // PCM24: canonical layout but a sample format the feeder refuses.
    final pcm24 = writeWavMono(tone, 48000);
    pcm24[34] = 24; // bitsPerSample 16 -> 24
    expect(
      await judge('pcm24.wav', pcm24),
      isFalse,
      reason: 'PCM24 is not PCM16/float32',
    );
  });

  test('splitWav/concatWav reject a compressed (ADPCM) WAV instead of '
      'byte-splicing it with PCM frame math', () async {
    // A structurally valid RIFF/WAVE whose fmt tag is MS ADPCM (2): its
    // blockAlign is a codec block, not a PCM frame, so byte-splicing would
    // produce silent nonsense. It must hit the "compressed audio" path.
    final dir = await Directory.systemTemp.createTemp('vocara_adpcm');
    addTearDown(() => dir.delete(recursive: true));

    final adpcm = _wavWithFmt(
      formatTag: 2, // WAVE_FORMAT_ADPCM
      channels: 1,
      sampleRate: 8000,
      blockAlign: 256, // a typical ADPCM codec-block size
      bits: 4,
      data: Uint8List.fromList(List.filled(4096, 0x55)),
    );
    final path = '${dir.path}/adpcm.wav';
    await File(path).writeAsBytes(adpcm);

    final edit = AudioEditService(files: _TempFiles(dir));
    expect(
      () => edit.splitWav(
        practiceId: 'P0000000000000000000000000',
        sourcePath: path,
        splitFileMs: 100,
      ),
      throwsA(isA<FormatException>()),
    );
    expect(
      () => edit.concatWav(
        practiceId: 'P0000000000000000000000000',
        firstPath: path,
        secondPath: path,
      ),
      throwsA(isA<FormatException>()),
    );
  });

  test(
    'splitWav rejects an EXTENSIBLE WAV whose SubFormat is not PCM/float',
    () async {
      // WAVE_FORMAT_EXTENSIBLE wrapping a compressed SubFormat (ADPCM = 2) must
      // be rejected just like a plainly-tagged compressed WAV.
      final dir = await Directory.systemTemp.createTemp('vocara_ext_adpcm');
      addTearDown(() => dir.delete(recursive: true));

      final bytes = _extensibleWavPcm16(
        Int16List.fromList(List.filled(4000, 1000)),
        8000,
      );
      // Patch the SubFormat's leading format code (fmt body offset 24; fmt body
      // starts at byte 20 in this fixed-layout file) from PCM (1) to ADPCM (2).
      bytes[20 + 24] = 2;
      final path = '${dir.path}/ext_adpcm.wav';
      await File(path).writeAsBytes(bytes);

      final edit = AudioEditService(files: _TempFiles(dir));
      expect(
        () => edit.splitWav(
          practiceId: 'P0000000000000000000000000',
          sourcePath: path,
          splitFileMs: 100,
        ),
        throwsA(isA<FormatException>()),
      );
    },
  );

  test(
    'splitWav rejects a header whose blockAlign lies about the frame size',
    () async {
      // Tag says PCM, but blockAlign != channels * bytes/sample: splitting on
      // such a header would shear every frame after the cut.
      final dir = await Directory.systemTemp.createTemp('vocara_badalign');
      addTearDown(() => dir.delete(recursive: true));

      final bad = _wavWithFmt(
        formatTag: 1,
        channels: 1,
        sampleRate: 8000,
        blockAlign: 3, // PCM16 mono must be 2
        bits: 16,
        data: Uint8List.fromList(List.filled(4000, 0)),
      );
      final path = '${dir.path}/bad.wav';
      await File(path).writeAsBytes(bad);

      final edit = AudioEditService(files: _TempFiles(dir));
      expect(
        () => edit.splitWav(
          practiceId: 'P0000000000000000000000000',
          sourcePath: path,
          splitFileMs: 100,
        ),
        throwsA(isA<FormatException>()),
      );
    },
  );

  test('splitWav still accepts an IEEE-float (format 3) WAV', () async {
    // The compressed-format rejection must not over-tighten: 32-bit float WAVs
    // are uncompressed and stay byte-spliceable.
    final dir = await Directory.systemTemp.createTemp('vocara_float');
    addTearDown(() => dir.delete(recursive: true));

    const sr = 8000;
    final data = Uint8List(sr * 4);
    final dv = ByteData.sublistView(data);
    for (var i = 0; i < sr; i++) {
      dv.setFloat32(i * 4, (i / sr) * 2 - 1, Endian.little);
    }
    final float = _wavWithFmt(
      formatTag: 3, // WAVE_FORMAT_IEEE_FLOAT
      channels: 1,
      sampleRate: sr,
      blockAlign: 4,
      bits: 32,
      data: data,
    );
    final path = '${dir.path}/float.wav';
    await File(path).writeAsBytes(float);

    final edit = AudioEditService(files: _TempFiles(dir));
    final parts = await edit.splitWav(
      practiceId: 'P0000000000000000000000000',
      sourcePath: path,
      splitFileMs: 250,
    );
    // 250 ms at 8 kHz = 2000 frames * 4 bytes. Data chunk = bytes 44..end for
    // this fixed canonical layout.
    final first = await File(parts.firstPath).readAsBytes();
    final second = await File(parts.secondPath).readAsBytes();
    expect(first.length - 44, 2000 * 4);
    expect(second.length - 44, (sr - 2000) * 4);
    // The float samples survive the byte splice untouched.
    final fd = ByteData.sublistView(first, 44);
    expect(fd.getFloat32(0, Endian.little), closeTo(-1.0, 1e-6));
    expect(
      fd.getFloat32(1999 * 4, Endian.little),
      closeTo((1999 / sr) * 2 - 1, 1e-6),
    );
  });

  test('split outputs of an odd-fmt-size WAV keep RIFF word alignment and '
      'round-trip through the service\'s own parsers', () async {
    // The re-emit paths preserve the source's fmt body verbatim. When that
    // body has an odd length, the RIFF spec requires a pad byte before the
    // next chunk — without it every later chunk sits at an odd offset, so the
    // service's own walkers (and symphonia's) never find `data` and the split
    // silently turns a playable clip into two unreadable files.
    final dir = await Directory.systemTemp.createTemp('vocara_oddfmt');
    addTearDown(() => dir.delete(recursive: true));

    const sr = 8000;
    final samples = Int16List.fromList(List.generate(sr, (i) => i - 4000));
    final srcPath = '${dir.path}/odd.wav';
    await File(srcPath).writeAsBytes(_oddFmtWavPcm16(samples, sr));

    final edit = AudioEditService(files: _TempFiles(dir));
    final parts = await edit.splitWav(
      practiceId: 'P0000000000000000000000000',
      sourcePath: srcPath,
      splitFileMs: 250,
    );

    for (final path in [parts.firstPath, parts.secondPath]) {
      final bytes = await File(path).readAsBytes();
      // RIFF(12) + fmt header(8) + fmt body(17) + pad(1) = 38.
      expect(
        String.fromCharCodes(bytes, 38, 42),
        'data',
        reason: 'the data id must sit at the word-aligned offset',
      );
      expect(
        readWavMono(bytes),
        isNotNull,
        reason: 'an emitted half must re-parse',
      );
    }
    final first = readWavMono(await File(parts.firstPath).readAsBytes())!;
    expect(first.samples.length, 2000); // 250 ms at 8 kHz

    // Full round-trip: merging the halves re-parses them and the merged file
    // re-parses in turn, with the audio intact.
    final merged = await edit.concatWav(
      practiceId: 'P0000000000000000000000000',
      firstPath: parts.firstPath,
      secondPath: parts.secondPath,
    );
    final round = readWavMono(await File(merged).readAsBytes());
    expect(round, isNotNull, reason: 'the merged file must re-parse');
    expect(round!.samples.length, sr);
    for (var i = 0; i < sr; i += 137) {
      expect((round.samples[i] * 32768).round(), samples[i]);
    }
  });

  test('trimWavHeadInPlace keeps an odd-fmt-size WAV word-aligned, so the '
      'trimmed take stays parseable and trimmable', () async {
    final dir = await Directory.systemTemp.createTemp('vocara_oddfmt_trim');
    addTearDown(() => dir.delete(recursive: true));

    final samples = Int16List.fromList(List.generate(1000, (i) => i));
    final path = '${dir.path}/odd.wav';
    await File(path).writeAsBytes(_oddFmtWavPcm16(samples, 1000));

    await trimWavHeadInPlace(path, 250);
    final pcm = readWavMono(await File(path).readAsBytes());
    expect(pcm, isNotNull, reason: 'the re-emitted header must re-parse');
    expect(pcm!.samples.length, 750);
    expect((pcm.samples.first * 32768).round(), 250);

    // A second trim parses the first trim's own output.
    await trimWavHeadInPlace(path, 250);
    final again = readWavMono(await File(path).readAsBytes());
    expect(again, isNotNull);
    expect(again!.samples.length, 500);
    expect((again.samples.first * 32768).round(), 500);
  });

  test('edit outputs land under an edits/ subdirectory, never directly in '
      'the recovery-scanned staging root', () async {
    // Boot recovery adopts orphan recording-shaped WAVs lying DIRECTLY under
    // audio/<pid>/; edit outputs written there would be resurrected as
    // phantom "(recovered)" takes after a kill between the file write and the
    // save commit. Split/concat/duplicate must therefore stage one level
    // down, under edits/ (still importable, still swept at boot).
    final dir = await Directory.systemTemp.createTemp('vocara_editsdir');
    addTearDown(() => dir.delete(recursive: true));

    const sr = 8000;
    final samples = Float64List.fromList([
      for (var i = 0; i < sr; i++) (i % 100) / 100 - 0.5,
    ]);
    final srcPath = '${dir.path}/src.wav';
    await File(srcPath).writeAsBytes(writeWavMono(samples, sr));

    final edit = AudioEditService(files: _TempFiles(dir));
    final parts = await edit.splitWav(
      practiceId: 'P0000000000000000000000000',
      sourcePath: srcPath,
      splitFileMs: 500,
    );
    final merged = await edit.concatWav(
      practiceId: 'P0000000000000000000000000',
      firstPath: parts.firstPath,
      secondPath: parts.secondPath,
    );
    final copy = await edit.duplicate(
      practiceId: 'P0000000000000000000000000',
      sourcePath: srcPath,
    );

    for (final path in [parts.firstPath, parts.secondPath, merged, copy]) {
      expect(
        File(path).parent.path,
        '${dir.path}/edits',
        reason: 'edit outputs must stage under edits/, not the staging root',
      );
      expect(await File(path).exists(), isTrue);
    }
  });

  test('splitWav rejects non-WAV audio', () async {
    final dir = await Directory.systemTemp.createTemp('vocara_edit');
    addTearDown(() => dir.delete(recursive: true));
    final bogus = '${dir.path}/song.m4a';
    await File(bogus).writeAsBytes(Uint8List.fromList(List.filled(2048, 7)));

    final edit = AudioEditService(files: _TempFiles(dir));
    expect(
      () => edit.splitWav(
        practiceId: 'P0000000000000000000000000',
        sourcePath: bogus,
        splitFileMs: 100,
      ),
      throwsA(isA<FormatException>()),
    );
  });

  group('trimWavHeadInPlace', () {
    test(
      'cuts exactly the head frames and preserves the sample format',
      () async {
        final dir = await Directory.systemTemp.createTemp('vocara_trim');
        addTearDown(() => dir.delete(recursive: true));
        // 1000 frames @ 1 kHz mono PCM16: sample i has value i, so the content
        // after a 250 ms trim must start at value 250 (placement correctness is
        // exactly "which frame is now file offset 0").
        final samples = Int16List.fromList(List.generate(1000, (i) => i));
        final path = '${dir.path}/take.wav';
        await File(path).writeAsBytes(
          _wavWithFmt(
            formatTag: 1,
            channels: 1,
            sampleRate: 1000,
            blockAlign: 2,
            bits: 16,
            data: samples.buffer.asUint8List(),
          ),
        );

        await trimWavHeadInPlace(path, 250);

        final pcm = readWavMono(await File(path).readAsBytes());
        expect(pcm, isNotNull);
        expect(pcm!.samples.length, 750);
        expect((pcm.samples.first * 32768).round(), 250);
        expect((pcm.samples.last * 32768).round(), 999);
      },
    );

    test('a trim past the end leaves an empty but valid WAV', () async {
      final dir = await Directory.systemTemp.createTemp('vocara_trim');
      addTearDown(() => dir.delete(recursive: true));
      final samples = Int16List.fromList(List.generate(100, (i) => i));
      final path = '${dir.path}/take.wav';
      await File(path).writeAsBytes(
        _wavWithFmt(
          formatTag: 1,
          channels: 1,
          sampleRate: 1000,
          blockAlign: 2,
          bits: 16,
          data: samples.buffer.asUint8List(),
        ),
      );

      await trimWavHeadInPlace(path, 5000);

      final pcm = readWavMono(await File(path).readAsBytes());
      expect(pcm, isNotNull);
      expect(pcm!.samples.length, 0);
    });

    test('zero and negative trims are no-ops', () async {
      final dir = await Directory.systemTemp.createTemp('vocara_trim');
      addTearDown(() => dir.delete(recursive: true));
      final samples = Int16List.fromList(List.generate(100, (i) => i));
      final path = '${dir.path}/take.wav';
      final bytes = _wavWithFmt(
        formatTag: 1,
        channels: 1,
        sampleRate: 1000,
        blockAlign: 2,
        bits: 16,
        data: samples.buffer.asUint8List(),
      );
      await File(path).writeAsBytes(bytes);

      await trimWavHeadInPlace(path, 0);
      await trimWavHeadInPlace(path, -10);

      expect(await File(path).readAsBytes(), bytes);
    });

    test('rejects non-WAV audio instead of corrupting it', () async {
      final dir = await Directory.systemTemp.createTemp('vocara_trim');
      addTearDown(() => dir.delete(recursive: true));
      final bogus = '${dir.path}/song.m4a';
      final junk = Uint8List.fromList(List.filled(2048, 7));
      await File(bogus).writeAsBytes(junk);

      await expectLater(
        () => trimWavHeadInPlace(bogus, 100),
        throwsA(isA<FormatException>()),
      );
      expect(await File(bogus).readAsBytes(), junk, reason: 'left untouched');
    });
  });
}
