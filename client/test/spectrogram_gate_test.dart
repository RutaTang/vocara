import 'dart:io';
import 'dart:math' as math;
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/audio_dsp.dart';
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

/// A canonical header with 8 channels of 32-bit samples (blockAlign 32,
/// consistent) — for exercising the absolute byte ceiling.
Uint8List _wavHeader8ch({
  required int sampleRate,
  required int declaredDataBytes,
}) {
  final b = BytesBuilder();
  void str(String s) => b.add(s.codeUnits);
  void u32(int v) =>
      b.add((ByteData(4)..setUint32(0, v, Endian.little)).buffer.asUint8List());
  void u16(int v) =>
      b.add((ByteData(2)..setUint16(0, v, Endian.little)).buffer.asUint8List());
  str('RIFF');
  u32(0xFFFFFFF0);
  str('WAVE');
  str('fmt ');
  u32(16);
  u16(1); // PCM
  u16(8); // channels
  u32(sampleRate);
  u32(sampleRate * 32);
  u16(32); // blockAlign = 8 * 4
  u16(32); // bits
  str('data');
  u32(declaredDataBytes);
  return b.toBytes();
}

void main() {
  // 10 min cap @ 44.1 kHz mono 16-bit = 44100 * 2 bytes * 600 s.
  const underCap = 44100 * 2 * 60; // 1 minute
  const overCap = 44100 * 2 * 660; // 11 minutes

  Future<File> write(Uint8List bytes) async {
    final dir = await Directory.systemTemp.createTemp('vocara_gate');
    addTearDown(() => dir.delete(recursive: true));
    final f = File('${dir.path}/probe.wav');
    await f.writeAsBytes(bytes);
    return f;
  }

  test('a canonical WAV under the cap is admitted', () async {
    // The gate clamps the declared payload to what the file physically holds,
    // so the admitted fixture needs its bytes actually present.
    final withBody = BytesBuilder()
      ..add(
        _wavHeader(
          sampleRate: 44100,
          blockAlign: 2,
          declaredDataBytes: underCap,
        ),
      )
      ..add(Uint8List(underCap));
    final f = await write(withBody.toBytes());
    expect(await SpectrogramService.admitForAnalysis(f), isNotNull);
  });

  test('an EXTENSIBLE WAV with extra chunks is still measured, and skipped '
      'over the cap', () async {
    // The old canonical-44-byte probe returned "unknown" for exactly this
    // shape and the whole file was then loaded; the chunk walk must both admit
    // a short one and refuse a long one.
    final shortBody = BytesBuilder()
      ..add(
        _wavHeader(
          sampleRate: 44100,
          blockAlign: 2,
          declaredDataBytes: underCap,
          extensible: true,
          extraListChunk: true,
        ),
      )
      ..add(Uint8List(underCap));
    final short = await write(shortBody.toBytes());
    expect(await SpectrogramService.admitForAnalysis(short), isNotNull);

    final long = await write(
      _wavHeader(
        sampleRate: 44100,
        blockAlign: 2,
        declaredDataBytes: overCap,
        extensible: true,
        extraListChunk: true,
      ),
    );
    expect(await SpectrogramService.admitForAnalysis(long), isNull);
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
      expect(await SpectrogramService.admitForAnalysis(f), isNull);
    },
  );

  test('non-WAV files are skipped without reading the body', () async {
    final f = await write(Uint8List.fromList(List.filled(4096, 7)));
    expect(await SpectrogramService.admitForAnalysis(f), isNull);
  });

  test('a missing file is skipped, not crashed on', () async {
    expect(
      await SpectrogramService.admitForAnalysis(File('/nonexistent/x.wav')),
      isNull,
    );
  });

  test(
    'a forged blockAlign cannot talk the frame count under the cap',
    () async {
      // channels=1, bits=16 → a truthful blockAlign is 2. A forged huge value
      // divides the declared size down to a tiny frame count that sails under
      // the duration cap — while the admitted READ would still be the full
      // multi-gigabyte data chunk. The consistency check must refuse it.
      final f = await write(
        _wavHeader(
          sampleRate: 44100,
          blockAlign: 4096, // lies: 1 channel * 16 bits is 2 bytes per frame
          declaredDataBytes: overCap,
        ),
      );
      expect(await SpectrogramService.admitForAnalysis(f), isNull);
    },
  );

  test(
    'headers outside the supported envelope are refused (absurd rate)',
    () async {
      final f = await write(
        _wavHeader(
          sampleRate:
              4000000, // no real recording; inflates every rate-derived cap
          blockAlign: 2,
          declaredDataBytes: underCap,
        ),
      );
      expect(await SpectrogramService.admitForAnalysis(f), isNull);
    },
  );

  test('the header-independent byte ceiling holds even when the per-rate '
      'duration math would admit', () async {
    // 8 channels of 32-bit at 44.1 kHz: blockAlign 32 is CONSISTENT, the
    // declared duration (4 min) is comfortably under the 10-minute cap AND
    // the frame count is under the frame ceiling — but the byte total
    // (~339 MB) crosses maxAnalysisBytes. Only the absolute byte ceiling can
    // refuse this header (the previous 850 MB fixture was ~10 min of audio
    // and got refused by the DURATION gate first, leaving the byte ceiling
    // untested).
    const declared = 4 * 60 * 44100 * 32; // 338,688,000 bytes
    assert(declared > SpectrogramService.maxAnalysisBytes);
    final header = _wavHeader8ch(
      sampleRate: 44100,
      declaredDataBytes: declared,
    );
    // The payload must PHYSICALLY exist: a header-only file is refused by the
    // physical-availability clamp regardless, which would leave the byte
    // ceiling untested. A sparse file (seek past the end, write one byte)
    // gives the declared length without 339 MB of disk I/O.
    final f = await write(header);
    final raf = await f.open(mode: FileMode.writeOnlyAppend);
    await raf.setPosition(header.length + declared - 1);
    await raf.writeByte(0);
    await raf.close();
    expect(
      await f.length(),
      header.length + declared,
      reason: 'the fixture must be physically as long as it declares',
    );
    expect(await SpectrogramService.admitForAnalysis(f), isNull);
  });

  test('the admitted length stops at the end of the data chunk: trailing '
      'junk after it is never read', () async {
    // A small honest data chunk followed by a pile of appended garbage: the
    // gate must admit the file but cap the read at the data chunk's end, so
    // the junk cannot ride into memory on the strength of the honest header.
    final header = _wavHeader(
      sampleRate: 44100,
      blockAlign: 2,
      declaredDataBytes: underCap,
    );
    final withJunk = BytesBuilder()
      ..add(header)
      ..add(Uint8List(underCap)) // the actual data payload
      ..add(Uint8List.fromList(List.filled(1 << 20, 0x5A))); // trailing junk
    final f = await write(withJunk.toBytes());

    final admission = await SpectrogramService.admitForAnalysis(f);
    expect(admission, isNotNull);
    expect(
      admission!.dataStart + admission.frames * 2,
      header.length + underCap,
      reason: 'the admitted PCM ends exactly at the data chunk boundary',
    );
    expect(
      admission.dataStart + admission.frames * 2 < await f.length(),
      isTrue,
      reason: 'the junk tail must sit past the admitted region',
    );
  });

  group('readWavMonoStreaming equals the reference decoder', () {
    Future<void> check(String name, Uint8List bytes) async {
      final f = await write(bytes);
      final admission = await SpectrogramService.admitForAnalysis(f);
      expect(admission, isNotNull, reason: '$name must be admitted');
      final streamed = SpectrogramService.readWavMonoStreaming(
        f.path,
        admission!,
      );
      final reference = readWavMono(bytes);
      expect(streamed, isNotNull, reason: name);
      expect(reference, isNotNull, reason: name);
      expect(streamed!.sampleRate, reference!.sampleRate, reason: name);
      expect(streamed.samples.length, reference.samples.length, reason: name);
      for (var i = 0; i < streamed.samples.length; i++) {
        expect(
          streamed.samples[i],
          closeTo(reference.samples[i], 1e-9),
          reason: '$name sample $i',
        );
      }
    }

    test('mono PCM16', () async {
      final samples = Float64List.fromList([
        for (var i = 0; i < 500; i++) math.sin(i / 25) * 0.8,
      ]);
      await check('mono pcm16', writeWavMono(samples, 8000));
    });

    test('EXTENSIBLE PCM16 with extra chunks (stereo downmix path)', () async {
      // Interleaved stereo with distinct channels, so the downmix averaging is
      // genuinely exercised.
      final data = Uint8List(400 * 4);
      final dv = ByteData.sublistView(data);
      for (var i = 0; i < 400; i++) {
        dv.setInt16(i * 4, (math.sin(i / 10) * 12000).round(), Endian.little);
        dv.setInt16(
          i * 4 + 2,
          (math.cos(i / 10) * 9000).round(),
          Endian.little,
        );
      }
      final b = BytesBuilder();
      void str(String s) => b.add(s.codeUnits);
      void u32(int v) => b.add(
        (ByteData(4)..setUint32(0, v, Endian.little)).buffer.asUint8List(),
      );
      void u16(int v) => b.add(
        (ByteData(2)..setUint16(0, v, Endian.little)).buffer.asUint8List(),
      );
      str('RIFF');
      u32(4 + 12 + (8 + 40) + (8 + data.length));
      str('WAVE');
      str('LIST');
      u32(4);
      b.add(Uint8List(4));
      str('fmt ');
      u32(40);
      u16(0xFFFE);
      u16(2); // channels
      u32(8000);
      u32(8000 * 4);
      u16(4); // blockAlign
      u16(16);
      u16(22);
      u16(16);
      u32(3);
      u16(1); // SubFormat = PCM
      b.add(const [
        0x00, 0x00, 0x00, 0x00, 0x10, 0x00, //
        0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71, //
      ]);
      str('data');
      u32(data.length);
      b.add(data);
      await check('extensible stereo pcm16', b.toBytes());
    });

    test('integer PCM64 is refused at the gate (no decoder branch exists: '
        'admitting it decoded a whole track of silence)', () async {
      final b = BytesBuilder();
      void str(String s) => b.add(s.codeUnits);
      void u32(int v) => b.add(
        (ByteData(4)..setUint32(0, v, Endian.little)).buffer.asUint8List(),
      );
      void u16(int v) => b.add(
        (ByteData(2)..setUint16(0, v, Endian.little)).buffer.asUint8List(),
      );
      str('RIFF');
      u32(36 + 80);
      str('WAVE');
      str('fmt ');
      u32(16);
      u16(1); // integer PCM…
      u16(1);
      u32(48000);
      u32(48000 * 8);
      u16(8); // blockAlign = 1 ch * 8 bytes
      u16(64); // …at 64 bits: not a real format
      str('data');
      u32(80);
      b.add(Uint8List(80));
      final f = await write(b.toBytes());
      expect(await SpectrogramService.admitForAnalysis(f), isNull);
    });

    test('non-finite float samples are scrubbed to zero, matching the '
        'reference decoder', () async {
      final data = Uint8List(6 * 4);
      final dv = ByteData.sublistView(data);
      dv.setFloat32(0, 0.5, Endian.little);
      dv.setFloat32(4, double.nan, Endian.little);
      dv.setFloat32(8, double.infinity, Endian.little);
      dv.setFloat32(12, double.negativeInfinity, Endian.little);
      dv.setFloat32(16, -0.25, Endian.little);
      dv.setFloat32(20, 0.125, Endian.little);
      final b = BytesBuilder();
      void str(String s) => b.add(s.codeUnits);
      void u32(int v) => b.add(
        (ByteData(4)..setUint32(0, v, Endian.little)).buffer.asUint8List(),
      );
      void u16(int v) => b.add(
        (ByteData(2)..setUint16(0, v, Endian.little)).buffer.asUint8List(),
      );
      str('RIFF');
      u32(36 + data.length);
      str('WAVE');
      str('fmt ');
      u32(16);
      u16(3);
      u16(1);
      u32(48000);
      u32(48000 * 4);
      u16(4);
      u16(32);
      str('data');
      u32(data.length);
      b.add(data);
      await check('float32 with NaN/Inf poison', b.toBytes());
      // Belt-and-braces beyond the equivalence: the poisoned slots really are
      // zero, not merely "whatever the reference produced".
      final f = await write(b.toBytes());
      final admission = (await SpectrogramService.admitForAnalysis(f))!;
      final pcm = SpectrogramService.readWavMonoStreaming(f.path, admission)!;
      expect(pcm.samples[1], 0.0);
      expect(pcm.samples[2], 0.0);
      expect(pcm.samples[3], 0.0);
      expect(pcm.samples[0], closeTo(0.5, 1e-9));
    });

    test('float32 (the take format)', () async {
      final data = Uint8List(300 * 4);
      final dv = ByteData.sublistView(data);
      for (var i = 0; i < 300; i++) {
        // Includes an over-unity peak, legal in float takes.
        dv.setFloat32(i * 4, math.sin(i / 7) * 1.2, Endian.little);
      }
      final b = BytesBuilder();
      void str(String s) => b.add(s.codeUnits);
      void u32(int v) => b.add(
        (ByteData(4)..setUint32(0, v, Endian.little)).buffer.asUint8List(),
      );
      void u16(int v) => b.add(
        (ByteData(2)..setUint16(0, v, Endian.little)).buffer.asUint8List(),
      );
      str('RIFF');
      u32(36 + data.length);
      str('WAVE');
      str('fmt ');
      u32(16);
      u16(3); // IEEE float
      u16(1);
      u32(48000);
      u32(48000 * 4);
      u16(4);
      u16(32);
      str('data');
      u32(data.length);
      b.add(data);
      await check('mono float32', b.toBytes());
    });
  });
}
