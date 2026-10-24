import 'dart:math' as math;
import 'dart:typed_data';

import 'package:fftea/fftea.dart';
import 'package:flutter/foundation.dart' show visibleForTesting;

import '../../domain/models/spectrogram.dart';
import '../../domain/models/voice_metrics.dart';

/// Decoded mono PCM plus its sample rate.
///
/// Samples are held as Float32: 16-bit PCM and float32 takes (every source
/// our own engine produces) are exactly representable, and the rare
/// float64/24/32-bit-integer WAV rounds to 24-bit mantissa precision — far
/// below any metric's resolution. The halved footprint of the ONE
/// full-length analysis buffer is the dominant analysis-memory term; all
/// windowed math downstream still runs in doubles.
class PcmAudio {
  PcmAudio(this.samples, this.sampleRate);
  final Float32List samples;
  final int sampleRate;
}

/// Encodes mono [samples] (in [-1, 1]) as a 16-bit PCM WAV. Isolate-safe.
Uint8List writeWavMono(List<double> samples, int sampleRate) {
  final n = samples.length;
  final dataSize = n * 2;
  final b = BytesBuilder();
  void str(String v) => b.add(v.codeUnits);
  void u32(int v) =>
      b.add((ByteData(4)..setUint32(0, v, Endian.little)).buffer.asUint8List());
  void u16(int v) =>
      b.add((ByteData(2)..setUint16(0, v, Endian.little)).buffer.asUint8List());
  str('RIFF');
  u32(36 + dataSize);
  str('WAVE');
  str('fmt ');
  u32(16);
  u16(1);
  u16(1);
  u32(sampleRate);
  u32(sampleRate * 2);
  u16(2);
  u16(16);
  str('data');
  u32(dataSize);
  final pcm = ByteData(dataSize);
  for (var i = 0; i < n; i++) {
    var v = (samples[i] * 32767).round();
    if (v > 32767) v = 32767;
    if (v < -32768) v = -32768;
    pcm.setInt16(i * 2, v, Endian.little);
  }
  b.add(pcm.buffer.asUint8List());
  return b.toBytes();
}

/// Parses an uncompressed WAV into mono samples: 16-bit PCM (legacy takes and
/// imports) or 32-bit IEEE float (what the recording engine writes — values may
/// exceed [-1, 1] and are passed through verbatim). Returns null for anything
/// else (e.g. compressed m4a/mp3). Isolate-safe.
PcmAudio? readWavMono(Uint8List bytes) {
  if (bytes.length < 44) return null;
  final bd = ByteData.sublistView(bytes);

  String tag(int o) => String.fromCharCodes(bytes, o, o + 4);
  if (tag(0) != 'RIFF' || tag(8) != 'WAVE') return null;

  var format = 1, channels = 1, sampleRate = 44100, bits = 16;
  var dataOffset = -1, dataLen = 0;

  var offset = 12;
  while (offset + 8 <= bytes.length) {
    final id = tag(offset);
    final size = bd.getUint32(offset + 4, Endian.little);
    final body = offset + 8;
    if (id == 'fmt ' && body + 16 <= bytes.length) {
      format = bd.getUint16(body, Endian.little);
      channels = bd.getUint16(body + 2, Endian.little);
      sampleRate = bd.getUint32(body + 4, Endian.little);
      bits = bd.getUint16(body + 14, Endian.little);
      // WAVE_FORMAT_EXTENSIBLE (e.g. AVFoundation recordings): the real format
      // tag lives in the SubFormat GUID's first two bytes.
      if (format == 0xFFFE && body + 26 <= bytes.length) {
        format = bd.getUint16(body + 24, Endian.little);
      }
    } else if (id == 'data') {
      dataOffset = body;
      dataLen = size;
      break;
    }
    offset = body + size + (size & 1); // chunks are word-aligned
  }

  final isPcm16 = format == 1 && bits == 16;
  final isFloat32 = format == 3 && bits == 32;
  if (dataOffset < 0 ||
      (!isPcm16 && !isFloat32) ||
      channels < 1 ||
      sampleRate <= 0) {
    return null;
  }

  final bytesPerSample = isPcm16 ? 2 : 4;
  final total = math.min(dataLen, bytes.length - dataOffset) ~/ bytesPerSample;
  final frames = total ~/ channels;
  final out = Float32List(frames);
  var p = dataOffset;
  for (var i = 0; i < frames; i++) {
    var sum = 0.0;
    for (var c = 0; c < channels; c++) {
      if (isPcm16) {
        sum += bd.getInt16(p, Endian.little) / 32768.0;
      } else {
        // Foreign float WAVs can carry NaN/inf samples; one would poison every
        // FFT/metric downstream. (Engine recordings are already sanitized on
        // write.) Read them as silence.
        final v = bd.getFloat32(p, Endian.little);
        sum += v.isFinite ? v : 0.0;
      }
      p += bytesPerSample;
    }
    out[i] = sum / channels;
    // Judge finiteness on the NARROWED value: a multi-channel sum of large
    // finite floats can overflow the Float32 store even though `sum` itself
    // was finite in double precision.
    if (!out[i].isFinite) out[i] = 0.0;
  }
  return PcmAudio(out, sampleRate);
}

/// The top of the range a [maxBin]-bin spectrogram actually covers — what both
/// builders record as [Spectrogram.maxHz]. Normally the requested [maxHz] (the
/// last bin sits at or just past it), but below 10 kHz the bins stop at Nyquist
/// first, and a maxHz left above that stretched the image's rows over a taller
/// axis, grid and formant overlay than they hold.
double _coveredMaxHz(double maxHz, int maxBin, int sampleRate, int fftSize) =>
    math.min(maxHz, (maxBin - 1) * sampleRate / fftSize);

/// Incremental spectrogram builder with EXACTLY [computeSpectrogram]'s
/// column math (same fftea STFT windowing — `stream`/`flush` share `run`'s
/// pipeline — same dB conversion, same assembly), fed in chunks so the
/// full-track sample buffer never has to exist. Pinned equal to the batch
/// path by test.
class StreamingSpectrogramBuilder {
  StreamingSpectrogramBuilder(
    this.sampleRate, {
    this.fftSize = 2048,
    this.hop = 512,
    this.maxHz = 5000,
  }) : maxBin = math.min(
         fftSize ~/ 2 + 1,
         (maxHz * fftSize / sampleRate).ceil() + 1,
       ),
       _stft = STFT(fftSize, Window.hanning(fftSize));

  final int sampleRate;
  final int fftSize;
  final int hop;
  final double maxHz;
  final int maxBin;
  final STFT _stft;
  final List<Float32List> _columns = [];
  bool _any = false;

  static const double _ln10 = 2.302585092994046;

  void _column(Float64x2List freq) {
    final mags = freq.discardConjugates().magnitudes();
    final col = Float32List(maxBin);
    for (var b = 0; b < maxBin; b++) {
      col[b] = 20.0 * (math.log(mags[b] + 1e-9) / _ln10);
    }
    _columns.add(col);
  }

  /// Feeds the next chunk of mono samples.
  void add(Float32List chunk) {
    if (chunk.isEmpty) return;
    _any = true;
    _stft.stream(chunk, _column, hop);
  }

  /// Flushes the trailing partial frame and assembles the [Spectrogram].
  /// Matches [computeSpectrogram] on empty input too: zero frames, never a
  /// lone zero-padded one.
  Spectrogram finish() {
    if (_any) _stft.flush(_column);
    final frames = _columns.length;
    final db = Float32List(frames * maxBin);
    var minDb = double.infinity;
    var maxDb = -double.infinity;
    for (var f = 0; f < frames; f++) {
      final col = _columns[f];
      final base = f * maxBin;
      for (var b = 0; b < maxBin; b++) {
        final v = col[b];
        db[base + b] = v;
        if (v < minDb) minDb = v;
        if (v > maxDb) maxDb = v;
      }
    }
    if (frames == 0) {
      minDb = -90;
      maxDb = 0;
    }
    return Spectrogram(
      frames: frames,
      bins: maxBin,
      maxHz: _coveredMaxHz(maxHz, maxBin, sampleRate, fftSize),
      sampleRate: sampleRate,
      fftSize: fftSize,
      hop: hop,
      db: db,
      minDb: minDb,
      maxDb: maxDb,
    );
  }
}

/// Computes a [Spectrogram] from mono [samples]. Pure + isolate-safe.
Spectrogram computeSpectrogram(
  Float32List samples,
  int sampleRate, {
  int fftSize = 2048,
  int hop = 512,
  double maxHz = 5000,
}) {
  final maxBin = math.min(
    fftSize ~/ 2 + 1,
    (maxHz * fftSize / sampleRate).ceil() + 1,
  );

  final stft = STFT(fftSize, Window.hanning(fftSize));
  final columns = <Float32List>[];
  const ln10 = 2.302585092994046;

  // Empty input → no frames, so VoiceMetrics.frames == Spectrogram.frames stays aligned
  // (computeVoiceMetrics returns 0 frames for empty input; fftea's STFT would otherwise
  // emit a single zero-padded frame here, breaking the per-frame overlay alignment).
  if (samples.isNotEmpty) {
    stft.run(samples, (Float64x2List freq) {
      final mags = freq.discardConjugates().magnitudes();
      final col = Float32List(maxBin);
      for (var b = 0; b < maxBin; b++) {
        col[b] = 20.0 * (math.log(mags[b] + 1e-9) / ln10);
      }
      columns.add(col);
    }, hop);
  }

  final frames = columns.length;
  final db = Float32List(frames * maxBin);
  var minDb = double.infinity;
  var maxDb = -double.infinity;
  for (var f = 0; f < frames; f++) {
    final col = columns[f];
    final base = f * maxBin;
    for (var b = 0; b < maxBin; b++) {
      final v = col[b];
      db[base + b] = v;
      if (v < minDb) minDb = v;
      if (v > maxDb) maxDb = v;
    }
  }
  if (frames == 0) {
    minDb = -90;
    maxDb = 0;
  }

  return Spectrogram(
    frames: frames,
    bins: maxBin,
    maxHz: _coveredMaxHz(maxHz, maxBin, sampleRate, fftSize),
    sampleRate: sampleRate,
    fftSize: fftSize,
    hop: hop,
    db: db,
    minDb: minDb,
    maxDb: maxDb,
  );
}

/// A zero-padded window copy: [count] values from [src] starting at absolute
/// index [start]; indices outside `[0, srcLen)` read as 0 (the exact
/// edge semantics the batch metrics loop always had).
Float32List _padSlice(Float32List src, int start, int count, int srcLen) {
  final out = Float32List(count);
  final from = start.clamp(0, srcLen);
  final to = (start + count).clamp(0, srcLen);
  for (var i = from; i < to; i++) {
    out[i - start] = src[i];
  }
  return out;
}

/// The per-frame metrics computation and its whole-take finish, shared by the
/// batch [computeVoiceMetrics] and the windowed [StreamingTakeAnalyzer] —
/// ONE function body, so the two paths cannot diverge (pinned bit-identical
/// by test). Callers extract the three zero-padded windows (raw fftSize,
/// decimated lpcWin+1 with one pre-emphasis lookback sample, yin yinWin) and
/// stream the raw samples through [feedClip]; everything else lives here.
class _MetricsPass {
  _MetricsPass(
    this.sampleRate, {
    required this.fftSize,
    required this.hop,
    required int capacityFrames,
  }) : half = fftSize ~/ 2,
       binHz = sampleRate / fftSize,
       window = Window.hanning(fftSize),
       fft = FFT(fftSize),
       dsFactor = (sampleRate / 11000).round().clamp(1, 8),
       yinFactor = (sampleRate / 22050).round().clamp(1, 2),
       ring = Float32List(capacityFrames),
       tilt = Float32List(capacityFrames),
       cpp = Float32List(capacityFrames),
       loudness = Float32List(capacityFrames),
       f0 = Float32List(capacityFrames),
       f1 = Float32List(capacityFrames),
       f2 = Float32List(capacityFrames),
       f3 = Float32List(capacityFrames),
       h1h2 = Float32List(capacityFrames),
       active = Uint8List(capacityFrames),
       energy = Float64List(capacityFrames),
       rf0 = Float32List(capacityFrames),
       rf1 = Float32List(capacityFrames),
       rf2 = Float32List(capacityFrames),
       rf3 = Float32List(capacityFrames),
       rh1h2 = Float32List(capacityFrames),
       per = Float32List(capacityFrames),
       voicedMask = Uint8List(capacityFrames),
       frame = Float64List(fftSize),
       logmag = Float64List(fftSize) {
    dsRate = sampleRate / dsFactor;
    yinRate = sampleRate / yinFactor;
    lpcOrder = (2 + dsRate / 1000).round().clamp(8, 20);
    lpcWin = fftSize ~/ dsFactor;
    lpcWindow = _hamming(lpcWin);
    yinWin = (fftSize / yinFactor).round();
    b2k = _binOf(2000);
    b4k = _binOf(4000);
    b50 = _binOf(50);
    b1k = _binOf(1000);
    b5k = _binOf(5000);
    qMin = (sampleRate / 500).floor().clamp(2, half);
    qMax = (sampleRate / 60).ceil().clamp(qMin + 1, half);
  }

  static const double _ln10 = 2.302585092994046;
  static const double preCoef = 0.97;

  final int sampleRate;
  final int fftSize;
  final int hop;
  final int half;
  final double binHz;
  final Float64List window;
  final FFT fft;
  final int dsFactor;
  final int yinFactor;
  late final double dsRate;
  late final double yinRate;
  late final int lpcOrder;
  late final int lpcWin;
  late final Float64List lpcWindow;
  late final int yinWin;
  late final int b2k, b4k, b50, b1k, b5k, qMin, qMax;

  // Per-frame outputs (capacity-sized; [emitted] is the truth).
  final Float32List ring, tilt, cpp, loudness, f0, f1, f2, f3, h1h2;
  final Uint8List active;
  final Float64List energy;
  final Float32List rf0, rf1, rf2, rf3, rh1h2, per;
  final Uint8List voicedMask;
  final Float64List frame, logmag;

  var _maxEnergy = 1e-12;
  var _emitted = 0;

  // Clip/peak scan state (streamed; the batch feeds the whole track once).
  var _len = 0;
  var _peak = 0.0;
  var _clipped = 0;
  var _run = 0;
  var _prevHigh = 0.0;

  int _binOf(double hz) {
    var b = (hz / binHz).round();
    if (b < 0) b = 0;
    if (b > half) b = half;
    return b;
  }

  static double _db(double mag) => 20.0 * (math.log(mag + 1e-9) / _ln10);

  /// Streams raw samples through the input-quality scan (peak / true-clipping
  /// run detection). Chunk boundaries are invisible: the run state carries.
  void feedClip(Float32List chunk) {
    const clipLevel = 0.997, clipRun = 4, flatEps = 0.0003;
    for (var i = 0; i < chunk.length; i++) {
      final a = chunk[i].abs();
      if (a > _peak) _peak = a;
      if (a >= clipLevel && (_run == 0 || (a - _prevHigh).abs() <= flatEps)) {
        _run++;
        _prevHigh = a;
        if (_run == clipRun) {
          _clipped += clipRun;
        } else if (_run > clipRun) {
          _clipped++;
        }
      } else {
        _run = 0;
      }
    }
    _len += chunk.length;
  }

  /// One analysis frame. [rawPadded] is the fftSize raw window (zero-padded
  /// past the end), [dsPadded] the lpcWin+1 decimated window whose first
  /// sample is the pre-emphasis lookback, [yinPadded] the yinWin decimated
  /// YIN window — all extracted by the caller with [_padSlice] semantics.
  void emitFrame(
    int fr,
    Float32List rawPadded,
    Float32List dsPadded,
    Float32List yinPadded,
  ) {
    assert(fr == _emitted, 'frames are emitted in order');
    _emitted = fr + 1;

    // --- Plain windowed magnitude spectrum (F18 / F19 / energy / CPP) +
    // the raw-RMS loudness (F9). ---
    var rawSq = 0.0;
    for (var i = 0; i < fftSize; i++) {
      final s = rawPadded[i];
      rawSq += s * s;
      frame[i] = s * window[i];
    }
    loudness[fr] = 20.0 * (math.log(math.sqrt(rawSq / fftSize) + 1e-9) / _ln10);
    final mags = fft.realFft(frame).discardConjugates().magnitudes();

    var e = 0.0;
    for (var b = 1; b <= half; b++) {
      e += mags[b] * mags[b];
    }
    energy[fr] = e;
    if (e > _maxEnergy) _maxEnergy = e;

    // F18 — ring / Singing Power Ratio. Skip the DC bin so a decode offset can
    // never be picked as the low-band peak.
    var lowPk = 1e-12, highPk = 1e-12;
    for (var b = 1; b < b2k; b++) {
      if (mags[b] > lowPk) lowPk = mags[b];
    }
    for (var b = b2k; b < b4k; b++) {
      if (mags[b] > highPk) highPk = mags[b];
    }
    ring[fr] = _db(highPk) - _db(lowPk);

    // F19 — spectral tilt / alpha ratio.
    var lowE = 0.0, highE = 0.0;
    for (var b = b50; b < b1k; b++) {
      lowE += mags[b] * mags[b];
    }
    for (var b = b1k; b < b5k; b++) {
      highE += mags[b] * mags[b];
    }
    tilt[fr] = 10.0 * (math.log((highE + 1e-12) / (lowE + 1e-12)) / _ln10);

    // F20 — CPP: real cepstrum of the dB spectrum, peak prominence above its
    // least-squares regression over the F0 quefrency window.
    for (var k = 0; k <= half; k++) {
      logmag[k] = _db(mags[k]);
    }
    for (var k = half + 1; k < fftSize; k++) {
      logmag[k] = logmag[fftSize - k];
    }
    final cep = fft.realFft(logmag);
    // Locate the rahmonic peak, then fit the cepstral-baseline regression over
    // the window EXCLUDING a small neighborhood of the peak — so the line
    // reflects the spectral trend, not the peak it's measured against (including
    // the peak biases prominence, and thus CPP, downward).
    var peakQ = qMin, peakV = -1e30;
    for (var q = qMin; q <= qMax; q++) {
      final v = cep[q].x / fftSize;
      if (v > peakV) {
        peakV = v;
        peakQ = q;
      }
    }
    const cppGuard = 3; // bins around the peak left out of the baseline fit
    var sx = 0.0, sy = 0.0, sxx = 0.0, sxy = 0.0, cnt = 0;
    for (var q = qMin; q <= qMax; q++) {
      if ((q - peakQ).abs() <= cppGuard) continue;
      final v = cep[q].x / fftSize;
      final qd = q.toDouble();
      sx += qd;
      sy += v;
      sxx += qd * qd;
      sxy += qd * v;
      cnt++;
    }
    final cntD = cnt.toDouble();
    final denom = cntD * sxx - sx * sx;
    final slope = denom.abs() < 1e-12 ? 0.0 : (cntD * sxy - sx * sy) / denom;
    final intercept = cnt == 0 ? 0.0 : (sy - slope * sx) / cntD;
    cpp[fr] = peakV - (slope * peakQ.toDouble() + intercept);

    // F0 — YIN on the decimated frame centered at this time: octave-robust and
    // time-domain, with a periodicity confidence that drives the voicing gate.
    final yin = yinPitch(yinPadded, yinRate);
    rf0[fr] = yin.f0;
    per[fr] = yin.periodicity;

    // H1−H2 register cue: peak dB at F0 vs at its 2nd harmonic (NaN unless a
    // clear in-band F0 whose 2nd harmonic is below Nyquist).
    final f0Hz = yin.f0;
    if (!f0Hz.isNaN && f0Hz > 0 && 2 * f0Hz < sampleRate / 2) {
      // Shrink the ±-bin search at low F0 so the F0 and 2·F0 windows can never
      // overlap (overlap lets H1's leakage inflate H2 and biases H1−H2 down).
      final hw = ((f0Hz / binHz - 1) / 2).floor().clamp(0, 2);
      rh1h2[fr] =
          _db(_harmonicPeak(mags, f0Hz / binHz, hw)) -
          _db(_harmonicPeak(mags, 2 * f0Hz / binHz, hw));
    } else {
      rh1h2[fr] = double.nan;
    }

    // F17 — formants via LPC on the decimated frame centered at this time, then
    // assign poles to the F1/F2/F3 bands (stable ordinal labels).
    final cands = _lpcFormantsPadded(
      dsPadded,
      dsRate,
      lpcOrder,
      lpcWin,
      lpcWindow,
      preCoef,
    );
    final (af1, af2, af3) = _assignFormants(cands);
    rf1[fr] = af1;
    rf2[fr] = af2;
    rf3[fr] = af3;
  }

  /// Whole-take post-processing (voicing gate, median smoothing, quality
  /// stats) — verbatim the batch tail, over the emitted frames.
  VoiceMetrics finish() {
    final nFrames = _emitted;
    Float32List f32(Float32List a) =>
        a.length == nFrames ? a : Float32List.sublistView(a, 0, nFrames);
    Uint8List u8(Uint8List a) =>
        a.length == nFrames ? a : Uint8List.sublistView(a, 0, nFrames);
    final ring = f32(this.ring),
        tilt = f32(this.tilt),
        cpp = f32(this.cpp),
        loudness = f32(this.loudness),
        f0 = f32(this.f0),
        f1 = f32(this.f1),
        f2 = f32(this.f2),
        f3 = f32(this.f3),
        h1h2 = f32(this.h1h2),
        rf0 = f32(this.rf0),
        rf1 = f32(this.rf1),
        rf2 = f32(this.rf2),
        rf3 = f32(this.rf3),
        rh1h2 = f32(this.rh1h2),
        per = f32(this.per);
    final active = u8(this.active), voicedMask = u8(this.voicedMask);

    // Voicing gate: a frame is voiced only when it is both audible (within 30 dB
    // of peak energy) AND periodic (YIN periodicity ≥ _voicedConfidence). Averages
    // use audible frames; F0/formants are nulled on every non-voiced frame so the
    // overlay never draws pitch/formant dots on noise or breath.
    final gate = _maxEnergy * 1e-3;
    var rSum = 0.0, tSum = 0.0, cSum = 0.0, n = 0;
    for (var fr = 0; fr < nFrames; fr++) {
      final audible = energy[fr] >= gate;
      final voiced = audible && per[fr] >= _voicedConfidence;
      // [active] tracks audible frames (LTAS), but the ring/tilt/CPP timbre
      // averages use only VOICED frames — averaging over breath/consonant noise
      // dilutes them and mismatched the live readout's voiced-only average.
      if (audible) active[fr] = 1;
      if (voiced) {
        voicedMask[fr] = 1;
        rSum += ring[fr];
        tSum += tilt[fr];
        cSum += cpp[fr];
        n++;
      } else {
        rf0[fr] = double.nan;
        rf1[fr] = double.nan;
        rf2[fr] = double.nan;
        rf3[fr] = double.nan;
        rh1h2[fr] = double.nan;
      }
    }
    // Temporal median smoothing (±2 frames) turns the raw F0/formants into steady
    // tracks, removing the per-frame jitter spectral peak-picking suffered from.
    _medianInto(rf0, f0, 2);
    _medianInto(rf1, f1, 2);
    _medianInto(rf2, f2, 2);
    _medianInto(rf3, f3, 2);
    _medianInto(rh1h2, h1h2, 2);
    // Re-apply the voicing gate AFTER smoothing. The two NaN meanings differ:
    // the median bridges measurement dropouts (LPC/H1−H2 misses on frames the
    // gate declared VOICED) from valid neighbours, while this mask keeps every
    // gate-NaN frame NaN so smoothing never paints F0/formants onto breath or
    // silence. Mirrors the live path's _smoothGated, which separates "unvoiced"
    // from "voiced but unmeasured" the same way.
    for (var fr = 0; fr < nFrames; fr++) {
      if (voicedMask[fr] == 0) {
        f0[fr] = double.nan;
        f1[fr] = double.nan;
        f2[fr] = double.nan;
        f3[fr] = double.nan;
        h1h2[fr] = double.nan;
      }
    }
    final inv = n == 0 ? 0.0 : 1.0 / n;

    // --- Input quality: peak level, clipping, SNR --------------------------
    final peakDbfs = _len == 0
        ? double.nan
        : 20.0 * (math.log(_peak + 1e-9) / _ln10);
    final clippedFraction = _len == 0 ? 0.0 : _clipped / _len;
    // SNR: median loudness of VOICED frames over the noise floor (the 10th-pct
    // of all frame loudness ≈ background between phrases).
    final allLoud = <double>[], voicedLoud = <double>[];
    for (var fr = 0; fr < nFrames; fr++) {
      final v = loudness[fr];
      if (!v.isFinite) continue;
      allLoud.add(v);
      if (active[fr] == 1 && per[fr] >= _voicedConfidence) voicedLoud.add(v);
    }
    var snrDb = double.nan;
    if (allLoud.length >= 8 && voicedLoud.isNotEmpty) {
      allLoud.sort();
      voicedLoud.sort();
      final noise = allLoud[(allLoud.length * 0.1).floor()];
      final signal = voicedLoud[voicedLoud.length ~/ 2];
      snrDb = signal - noise;
    }

    return VoiceMetrics(
      frames: nFrames,
      hop: hop,
      sampleRate: sampleRate,
      ring: ring,
      tilt: tilt,
      cpp: cpp,
      loudness: loudness,
      f0: f0,
      f1: f1,
      f2: f2,
      f3: f3,
      h1h2: h1h2,
      active: active,
      ringAvg: rSum * inv,
      tiltAvg: tSum * inv,
      cppAvg: cSum * inv,
      peakDbfs: peakDbfs,
      clippedFraction: clippedFraction,
      snrDb: snrDb,
    );
  }
}

/// A 3-stage Butterworth decimator producing [_decimate]'s output
/// INCREMENTALLY: each incoming sample runs the cascade per stage with the
/// same Float32 rounding between stages the in-place batch filter has (each
/// batch stage reads the previous stage's f32-stored output), so the
/// streamed decimated signal is bit-identical to the batch copy.
class _StreamDecimator {
  _StreamDecimator(this.factor, double fs)
    : assert(factor >= 1),
      _stages = List.generate(3, (_) => _BiquadState()) {
    final fc = 0.45 * fs / factor;
    final w0 = 2 * math.pi * fc / fs;
    final cosw = math.cos(w0), sinw = math.sin(w0);
    final alpha = sinw / math.sqrt2;
    final a0 = 1 + alpha;
    _b0 = (1 - cosw) / 2 / a0;
    _b1 = (1 - cosw) / a0;
    _b2 = _b0;
    _a1 = -2 * cosw / a0;
    _a2 = (1 - alpha) / a0;
  }

  final int factor;
  final List<_BiquadState> _stages;
  late final double _b0, _b1, _b2, _a1, _a2;
  var _phase = 0;

  /// Feeds one raw sample; appends the decimated output sample to [out]
  /// when this position lands on the decimation grid.
  void feed(double x, void Function(double) out) {
    var v = x;
    if (factor > 1) {
      for (final st in _stages) {
        // Round to Float32 between stages, matching the batch's in-place
        // Float32List storage exactly.
        v = _f32(st.step(v, _b0, _b1, _b2, _a1, _a2));
      }
    }
    if (_phase == 0) out(v);
    _phase = (_phase + 1) % factor;
  }

  static final Float32List _round = Float32List(1);
  static double _f32(double v) {
    _round[0] = v;
    return _round[0];
  }
}

class _BiquadState {
  var x1 = 0.0, x2 = 0.0, y1 = 0.0, y2 = 0.0;

  double step(
    double xi,
    double b0,
    double b1,
    double b2,
    double a1,
    double a2,
  ) {
    final yi = b0 * xi + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
    x2 = x1;
    x1 = xi;
    y2 = y1;
    y1 = yi;
    return yi;
  }
}

/// A growable append-only Float32 buffer with COMPACTION: absolute indexing
/// survives dropping the consumed head, so the streaming analyzer's rolling
/// windows never hold more than the lookback they need.
class _RollingF32 {
  var _buf = Float32List(1 << 15);
  var _base = 0; // absolute index of _buf[0]
  var _len = 0; // absolute total appended

  int get length => _len;

  void add(double v) {
    final local = _len - _base;
    if (local >= _buf.length) {
      final grown = Float32List(_buf.length * 2);
      grown.setRange(0, local, _buf);
      _buf = grown;
    }
    _buf[local] = v;
    _len++;
  }

  /// Value at absolute index [i]; 0 outside `[base, len)` — the zero-pad
  /// edge semantics of [_padSlice].
  double at(int i) {
    if (i < _base || i >= _len) return 0;
    return _buf[i - _base];
  }

  /// Drops everything below absolute index [keepFrom] (clamped), compacting
  /// the storage when the dead head has grown past a block.
  void dropBelow(int keepFrom) {
    final target = keepFrom.clamp(_base, _len);
    final dead = target - _base;
    if (dead < (1 << 14)) return;
    final live = _len - target;
    final next = Float32List(math.max(1 << 15, _nextPow2(live + (1 << 15))));
    for (var i = 0; i < live; i++) {
      next[i] = _buf[dead + i];
    }
    _buf = next;
    _base = target;
  }

  static int _nextPow2(int v) {
    var p = 1;
    while (p < v) {
      p <<= 1;
    }
    return p;
  }

  /// Zero-padded window copy; [maxLen] caps the readable length (indices at
  /// or past it read 0 — the batch-equivalent signal length, see the
  /// decimator tail note in [StreamingTakeAnalyzer]).
  Float32List slice(int start, int count, int maxLen) {
    final out = Float32List(count);
    for (var i = 0; i < count; i++) {
      final idx = start + i;
      out[i] = idx >= maxLen ? 0 : at(idx);
    }
    return out;
  }
}

/// WINDOWED streaming take analysis: feeds mono chunks, computes the
/// spectrogram (optional) and the full [VoiceMetrics] without the whole-track
/// buffer or the full decimated copies ever existing. The per-frame math IS
/// [_MetricsPass] — the same code the batch [computeVoiceMetrics] runs — and
/// the decimators reproduce [_decimate] bit-exactly, so the result is pinned
/// bit-identical to the batch path by test (batch = the equality oracle).
///
/// Rolling lookback: raw needs `[fr*hop, fr*hop + fftSize)`, the decimated
/// signals a centered window each — a frame is emitted the moment all three
/// are available, and consumed head is dropped, so the resident set is a few
/// windows regardless of take length.
class StreamingTakeAnalyzer {
  StreamingTakeAnalyzer(
    this.sampleRate, {
    this.fftSize = 2048,
    this.hop = 512,
    required int expectedFrames,
    bool spectrogram = true,
    double maxHz = 5000,
  }) : _pass = _MetricsPass(
         sampleRate,
         fftSize: fftSize,
         hop: hop,
         capacityFrames: expectedFrames,
       ),
       _spectro = spectrogram
           ? StreamingSpectrogramBuilder(
               sampleRate,
               fftSize: fftSize,
               hop: hop,
               maxHz: maxHz,
             )
           : null {
    _dsDec = _StreamDecimator(_pass.dsFactor, sampleRate.toDouble());
    _yinDec = _StreamDecimator(_pass.yinFactor, sampleRate.toDouble());
  }

  final int sampleRate;
  final int fftSize;
  final int hop;
  final _MetricsPass _pass;
  final StreamingSpectrogramBuilder? _spectro;
  late final _StreamDecimator _dsDec;
  late final _StreamDecimator _yinDec;
  final _raw = _RollingF32();
  final _ds = _RollingF32();
  final _yin = _RollingF32();
  var _nextFrame = 0;

  /// Feeds the next chunk of mono samples.
  void add(Float32List chunk) {
    _pass.feedClip(chunk);
    _spectro?.add(chunk);
    for (var i = 0; i < chunk.length; i++) {
      final s = chunk[i];
      _raw.add(s);
      _dsDec.feed(s, _ds.add);
      _yinDec.feed(s, _yin.add);
    }
    _emitReady(flush: false);
  }

  void _emitReady({required bool flush}) {
    final len = _raw.length;
    // The frame count is fixed by the FINAL length; during feeding only
    // frames whose inputs are complete may emit (they exist for any longer
    // final length too).
    final nFrames = len == 0
        ? 0
        : (len <= fftSize ? 1 : ((len - fftSize + hop - 1) ~/ hop) + 1);
    while (_nextFrame < nFrames) {
      final fr = _nextFrame;
      final start = fr * hop;
      final dsCenter = ((fr * hop + fftSize / 2) / _pass.dsFactor).round();
      final yinCenter = ((fr * hop + fftSize / 2) / _pass.yinFactor).round();
      // The BATCH decimated length is len over the factor (floored); the
      // incremental decimator can be one provisional sample ahead when len
      // is not a multiple of the factor. Readiness AND extraction cap at the
      // batch-equivalent length, so a tail frame never reads a sample the
      // batch would have zero-padded (bit-exactness at the edges).
      final dsMax = len ~/ _pass.dsFactor;
      final yinMax = _pass.yinFactor == 1 ? len : len ~/ _pass.yinFactor;
      if (!flush) {
        final rawReady = start + fftSize <= len;
        final dsReady =
            dsCenter - _pass.lpcWin ~/ 2 - 1 + _pass.lpcWin + 1 <= dsMax;
        final yinReady = yinCenter - _pass.yinWin ~/ 2 + _pass.yinWin <= yinMax;
        if (!rawReady || !dsReady || !yinReady) return;
      }
      _pass.emitFrame(
        fr,
        _raw.slice(start, fftSize, len),
        _ds.slice(dsCenter - _pass.lpcWin ~/ 2 - 1, _pass.lpcWin + 1, dsMax),
        _yin.slice(yinCenter - _pass.yinWin ~/ 2, _pass.yinWin, yinMax),
      );
      _nextFrame = fr + 1;
      // Retain only what future frames can still reach.
      final nextStart = _nextFrame * hop;
      _raw.dropBelow(nextStart);
      final nextDsCenter = ((_nextFrame * hop + fftSize / 2) / _pass.dsFactor)
          .round();
      _ds.dropBelow(nextDsCenter - _pass.lpcWin ~/ 2 - 1);
      final nextYinCenter = ((_nextFrame * hop + fftSize / 2) / _pass.yinFactor)
          .round();
      _yin.dropBelow(nextYinCenter - _pass.yinWin ~/ 2);
    }
  }

  /// Flushes the tail (frames whose windows extend past the end, zero-padded
  /// exactly like the batch) and assembles the results.
  ({VoiceMetrics metrics, Spectrogram? spectrogram}) finish() {
    _emitReady(flush: true);
    return (metrics: _pass.finish(), spectrogram: _spectro?.finish());
  }
}

/// Computes per-frame voice-timbre metrics from mono [samples] — ring/SPR (F18),
/// spectral tilt / alpha ratio (F19), cepstral peak prominence (F20) and the
/// first three formants F1/F2/F3 (F17). Same framing as [computeSpectrogram].
/// Pure + isolate-safe. See [VoiceMetrics] for the meaning of each curve.
/// The per-frame math lives in [_MetricsPass], shared verbatim with the
/// windowed [StreamingTakeAnalyzer] (this batch path doubles as its
/// equality oracle).
VoiceMetrics computeVoiceMetrics(
  Float32List samples,
  int sampleRate, {
  int fftSize = 2048,
  int hop = 512,
}) {
  // Match fftea's STFT framing exactly (computeSpectrogram uses STFT.run, which
  // zero-pads and emits a trailing partial frame) so VoiceMetrics.frames ==
  // Spectrogram.frames and the per-frame overlay/playhead stay aligned. The
  // final frame is zero-padded past the end of [samples].
  final len = samples.length;
  final nFrames = len == 0
      ? 0
      : (len <= fftSize ? 1 : ((len - fftSize + hop - 1) ~/ hop) + 1);
  final pass = _MetricsPass(
    sampleRate,
    fftSize: fftSize,
    hop: hop,
    capacityFrames: nFrames,
  );
  pass.feedClip(samples);

  // Whole-track decimated copies (the streaming path produces these
  // incrementally with identical filter math; see StreamingTakeAnalyzer).
  final ds = _decimate(samples, pass.dsFactor, sampleRate.toDouble());
  final yinSig = pass.yinFactor == 1
      ? samples
      : _decimate(samples, pass.yinFactor, sampleRate.toDouble());

  for (var fr = 0; fr < nFrames; fr++) {
    final start = fr * hop;
    final dsCenter = ((fr * hop + fftSize / 2) / pass.dsFactor).round();
    final yinCenter = ((fr * hop + fftSize / 2) / pass.yinFactor).round();
    pass.emitFrame(
      fr,
      _padSlice(samples, start, fftSize, len),
      _padSlice(
        ds,
        dsCenter - pass.lpcWin ~/ 2 - 1,
        pass.lpcWin + 1,
        ds.length,
      ),
      _padSlice(
        yinSig,
        yinCenter - pass.yinWin ~/ 2,
        pass.yinWin,
        yinSig.length,
      ),
    );
  }
  return pass.finish();
}

// ---- F17 formant estimation (LPC) ---------------------------------------

/// A Hamming window of length [n].
Float64List _hamming(int n) {
  final w = Float64List(n);
  if (n <= 1) {
    if (n == 1) w[0] = 1.0;
    return w;
  }
  for (var i = 0; i < n; i++) {
    w[i] = 0.54 - 0.46 * math.cos(2 * math.pi * i / (n - 1));
  }
  return w;
}

/// One 2nd-order Butterworth low-pass (RBJ biquad, Q≈0.707), IN PLACE.
/// Direct Form I stays exact when writing over the input: each output needs
/// x[i] plus the two PREVIOUS raw inputs, which live in the x1/x2 scalars —
/// x[i] is captured before it is overwritten, so nothing downstream reads a
/// filtered value as a raw one. The recursion state (x1/x2/y1/y2) and every
/// intermediate stay double; only the stored output rounds to Float32.
void _biquadLpInPlace(Float32List x, double fc, double fs) {
  final w0 = 2 * math.pi * fc / fs;
  final cosw = math.cos(w0), sinw = math.sin(w0);
  final alpha = sinw / math.sqrt2;
  final a0 = 1 + alpha;
  final b0 = (1 - cosw) / 2 / a0;
  final b1 = (1 - cosw) / a0;
  final b2 = b0;
  final a1 = -2 * cosw / a0;
  final a2 = (1 - alpha) / a0;
  var x1 = 0.0, x2 = 0.0, y1 = 0.0, y2 = 0.0;
  for (var i = 0; i < x.length; i++) {
    final xi = x[i];
    final yi = b0 * xi + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
    x[i] = yi;
    x2 = x1;
    x1 = xi;
    y2 = y1;
    y1 = yi;
  }
}

/// Anti-aliased decimation by integer [factor] (three cascaded Butterworth
/// LPs). MEMORY: exactly one working copy of [x] — the three filter stages
/// run in place on it. The old shape chained three allocating stages, so a
/// long take transiently held three extra full-length buffers; on the
/// analysis path that tripled the dominant term of the whole compute's peak.
/// [x] itself is never mutated (callers keep using the undecimated samples).
Float32List _decimate(Float32List x, int factor, double fs) {
  if (factor <= 1) return x;
  // Anti-alias below the new Nyquist. Three cascaded biquads (~6th order) give
  // markedly more stopband attenuation than two, so audible energy above the
  // fold doesn't alias into the formant bands. fc = 0.45·newSampleRate, i.e.
  // 0.9·newNyquist, so F3 (≤ ~3.8 kHz) is still well inside the passband.
  final fc = 0.45 * fs / factor;
  final lp = Float32List.fromList(x);
  _biquadLpInPlace(lp, fc, fs);
  _biquadLpInPlace(lp, fc, fs);
  _biquadLpInPlace(lp, fc, fs);
  final m = x.length ~/ factor;
  final out = Float32List(m);
  for (var i = 0; i < m; i++) {
    out[i] = lp[i * factor];
  }
  return out;
}

/// Levinson-Durbin recursion: LPC coefficients [1, a1..ap] for the all-pole
/// model A(z)=1+Σ a_k·z⁻ᵏ, or null if the frame is degenerate.
List<double>? _levinson(Float64List r, int order) {
  final a = List<double>.filled(order + 1, 0.0);
  a[0] = 1.0;
  var err = r[0];
  if (err <= 0) return null;
  for (var i = 1; i <= order; i++) {
    var acc = r[i];
    for (var j = 1; j < i; j++) {
      acc += a[j] * r[i - j];
    }
    final k = -acc / err;
    final prev = List<double>.from(a);
    for (var j = 1; j < i; j++) {
      a[j] = prev[j] + k * prev[i - j];
    }
    a[i] = k;
    err *= 1 - k * k;
    if (err <= 0) return null;
  }
  return a;
}

/// Durand-Kerner: all complex roots of the monic polynomial with descending
/// coefficients [coeffs] (coeffs[0]==1). Returns [re, im].
List<List<double>> _durandKerner(List<double> coeffs) {
  final p = coeffs.length - 1;
  final re = List<double>.filled(p, 0.0);
  final im = List<double>.filled(p, 0.0);
  // Initial guesses equally spaced on a circle (offset off the real axis) —
  // spreads the roots so coincident-guess stalls are unlikely.
  for (var k = 0; k < p; k++) {
    final ang = 2 * math.pi * (k + 0.5) / p;
    re[k] = 0.9 * math.cos(ang);
    im[k] = 0.9 * math.sin(ang);
  }
  for (var iter = 0; iter < 60; iter++) {
    var maxStep = 0.0;
    for (var k = 0; k < p; k++) {
      final zr = re[k], zi = im[k];
      // P(z_k) via Horner.
      var pr = coeffs[0], pi = 0.0;
      for (var i = 1; i <= p; i++) {
        final nr = pr * zr - pi * zi + coeffs[i];
        final ni = pr * zi + pi * zr;
        pr = nr;
        pi = ni;
      }
      // denom = Π_{j≠k}(z_k - z_j).
      var dr = 1.0, di = 0.0;
      for (var j = 0; j < p; j++) {
        if (j == k) continue;
        final xr = zr - re[j], xi = zi - im[j];
        final nr = dr * xr - di * xi;
        final ni = dr * xi + di * xr;
        dr = nr;
        di = ni;
      }
      final dd = dr * dr + di * di;
      if (dd < 1e-30) {
        // Near-coincident roots stall this update — nudge and keep iterating so
        // a stuck root isn't mistaken for convergence.
        re[k] = zr + 1e-3 * (k + 1);
        im[k] = zi + 1e-3;
        maxStep = 1.0;
        continue;
      }
      final qr = (pr * dr + pi * di) / dd;
      final qi = (pi * dr - pr * di) / dd;
      re[k] = zr - qr;
      im[k] = zi - qi;
      final step = qr * qr + qi * qi;
      if (step > maxStep) maxStep = step;
    }
    if (maxStep < 1e-18) break;
  }
  return [re, im];
}

/// LPC formant candidates (sorted Hz) for the frame of [ds] (rate [dsRate])
/// centered at sample [center]: pre-emphasis → Hamming → autocorrelation →
/// Levinson → polynomial roots → keep narrow-bandwidth in-range poles.
List<double> _lpcFormants(
  Float32List ds,
  double dsRate,
  int order,
  int win,
  Float64List window,
  double preCoef,
  int center,
) {
  if (ds.length < 2) return const [];
  final start = center - win ~/ 2;
  return _lpcFormantsPadded(
    _padSlice(ds, start - 1, win + 1, ds.length),
    dsRate,
    order,
    win,
    window,
    preCoef,
  );
}

/// [_lpcFormants] on a pre-extracted window: [padded] holds `win + 1`
/// zero-padded samples whose FIRST value is the pre-emphasis lookback
/// (`ds[start - 1]`). Shared by the batch and streaming metrics paths.
List<double> _lpcFormantsPadded(
  Float32List padded,
  double dsRate,
  int order,
  int win,
  Float64List window,
  double preCoef,
) {
  final frame = Float64List(win);
  var prev = padded[0];
  var energy = 0.0;
  for (var i = 0; i < win; i++) {
    final s = padded[1 + i];
    frame[i] = (s - preCoef * prev) * window[i];
    prev = s;
    energy += frame[i] * frame[i];
  }
  if (energy <= 1e-12) return const [];

  final r = Float64List(order + 1);
  for (var lag = 0; lag <= order; lag++) {
    var acc = 0.0;
    for (var i = lag; i < win; i++) {
      acc += frame[i] * frame[i - lag];
    }
    r[lag] = acc;
  }
  if (r[0] <= 0) return const [];
  r[0] *= 1.0 + 1e-6; // ridge for numerical stability

  final a = _levinson(r, order);
  if (a == null) return const [];

  final roots = _durandKerner(a);
  final re = roots[0], im = roots[1];
  final nyq = dsRate / 2;
  final out = <double>[];
  for (var k = 0; k < re.length; k++) {
    if (im[k] <= 0) continue; // upper half-plane only (skip conjugates)
    final mag = math.sqrt(re[k] * re[k] + im[k] * im[k]);
    // Allow magnitudes a hair over 1 so sharp formants whose pole rounds onto
    // the unit circle aren't dropped; clearly-unstable poles are still rejected.
    if (mag <= 0 || mag > 1.001) continue;
    final freq = math.atan2(im[k], re[k]) * dsRate / (2 * math.pi);
    if (freq < 90 || freq > nyq - 100) continue;
    final bw = -dsRate / math.pi * math.log(mag);
    if (bw > 400) continue; // wide => not a formant
    out.add(freq);
  }
  out.sort();
  return out;
}

/// Peak magnitude within ±[halfWidth] bins of [centerBin] (finds the actual
/// harmonic peak despite coarse FFT resolution). A genuine harmonic's energy is
/// spread across neighbouring bins by the Hann window, so once the peak bin is
/// located its true inter-bin amplitude is recovered by parabolic interpolation
/// of the log-magnitude — otherwise H1/H2 are read low and biased. Returns 0 if
/// out of range.
double _harmonicPeak(List<double> mags, double centerBin, int halfWidth) {
  final c = centerBin.round();
  var peak = 0.0;
  var peakBin = -1;
  for (var b = c - halfWidth; b <= c + halfWidth; b++) {
    if (b >= 0 && b < mags.length && mags[b] > peak) {
      peak = mags[b];
      peakBin = b;
    }
  }
  if (peakBin <= 0 || peakBin >= mags.length - 1) return peak;
  const eps = 1e-12;
  final l = math.log(mags[peakBin - 1] + eps);
  final m = math.log(mags[peakBin] + eps);
  final r = math.log(mags[peakBin + 1] + eps);
  final denom = l - 2 * m + r;
  if (denom >= 0) return peak; // not a convex peak — keep the raw bin value
  final delta = 0.5 * (l - r) / denom;
  return math.exp(m - 0.25 * (l - r) * delta);
}

/// Assigns sorted formant candidates [cands] (Hz) jointly to (F1, F2, F3): the
/// lexicographically-smallest in-band, ordered, ≥150 Hz-separated triple. Unlike
/// the old greedy lowest-in-band pick, a candidate is only labeled F1 if a valid
/// F2/F3 exists above it — so a missing/spurious low pole can't cascade into a
/// mislabeled F2/F3. Falls back to the best F1+F2 pair, then F1 alone.
(double, double, double) _assignFormants(List<double> cands) {
  const f1Lo = 200.0, f1Hi = 1100.0;
  const f2Lo = 700.0, f2Hi = 2700.0;
  const f3Lo = 1600.0, f3Hi = 3800.0;
  const sep = 150.0;
  final n = cands.length;
  // First full triple (cands sorted ascending ⇒ lexicographically smallest).
  for (var i = 0; i < n; i++) {
    final c1 = cands[i];
    if (c1 < f1Lo || c1 > f1Hi) continue;
    for (var j = i + 1; j < n; j++) {
      final c2 = cands[j];
      if (c2 < f2Lo || c2 > f2Hi || c2 < c1 + sep) continue;
      for (var k = j + 1; k < n; k++) {
        final c3 = cands[k];
        if (c3 >= f3Lo && c3 <= f3Hi && c3 >= c2 + sep) {
          return (c1, c2, c3);
        }
      }
    }
  }
  // Best F1+F2 pair.
  for (var i = 0; i < n; i++) {
    final c1 = cands[i];
    if (c1 < f1Lo || c1 > f1Hi) continue;
    for (var j = i + 1; j < n; j++) {
      final c2 = cands[j];
      if (c2 >= f2Lo && c2 <= f2Hi && c2 >= c1 + sep) {
        return (c1, c2, double.nan);
      }
    }
  }
  // F1 alone (returns on the first in-band F1 ⇒ only reached when an F1 exists).
  for (final c1 in cands) {
    if (c1 >= f1Lo && c1 <= f1Hi) return (c1, double.nan, double.nan);
  }
  // No in-band F1 at all: still report F2/F3 (a high voice, or a missed F1,
  // must not drop the upper formants — this matches the old greedy behavior).
  for (var j = 0; j < n; j++) {
    final c2 = cands[j];
    if (c2 < f2Lo || c2 > f2Hi) continue;
    for (var k = j + 1; k < n; k++) {
      final c3 = cands[k];
      if (c3 >= f3Lo && c3 <= f3Hi && c3 >= c2 + sep) {
        return (double.nan, c2, c3);
      }
    }
  }
  for (final c2 in cands) {
    if (c2 >= f2Lo && c2 <= f2Hi) return (double.nan, c2, double.nan);
  }
  return (double.nan, double.nan, double.nan);
}

/// Writes the running ±[half]-frame median of [src] (ignoring NaN) into [dst].
/// A NaN center is bridged from its valid neighbours on purpose: the raw
/// tracks carry NaN on VOICED frames too (LPC / H1−H2 measurement dropouts),
/// and those one-frame holes should be filled. Keeping unvoiced frames NaN is
/// the CALLER's job — it re-applies the voicing mask after smoothing.
void _medianInto(Float32List src, Float32List dst, int half) {
  final n = src.length;
  final buf = <double>[];
  for (var i = 0; i < n; i++) {
    buf.clear();
    final lo = i - half < 0 ? 0 : i - half;
    final hi = i + half >= n ? n - 1 : i + half;
    for (var j = lo; j <= hi; j++) {
      final v = src[j];
      if (!v.isNaN) buf.add(v);
    }
    if (buf.isEmpty) {
      dst[i] = double.nan;
    } else {
      buf.sort();
      dst[i] = buf[buf.length ~/ 2];
    }
  }
}

/// Incremental STFT for live audio: feed samples as they arrive, get one dB
/// column (0..[maxHz]) per [hop] samples. Runs on the main thread (each FFT is
/// sub-millisecond). Pure Dart — also unit-testable without a mic.
class StreamingStft {
  StreamingStft({
    this.fftSize = 2048,
    this.hop = 512,
    this.sampleRate = 44100,
    this.maxHz = 5000,
  }) : maxBin = math.min(
         fftSize ~/ 2 + 1,
         (maxHz * fftSize / sampleRate).ceil() + 1,
       ),
       _window = Window.hanning(fftSize),
       _fft = FFT(fftSize);

  final int fftSize;
  final int hop;
  final int sampleRate;
  final double maxHz;
  final int maxBin;

  final Float64List _window;
  final FFT _fft;
  final List<double> _acc = <double>[];

  double get binHz => sampleRate / fftSize;

  /// Appends [samples] and emits a dB column via [onColumn] for every [hop].
  void addSamples(
    Float32List samples,
    void Function(Float32List col) onColumn,
  ) {
    _acc.addAll(samples);
    const ln10 = 2.302585092994046;
    final frame = Float64List(fftSize);
    while (_acc.length >= fftSize) {
      for (var i = 0; i < fftSize; i++) {
        frame[i] = _acc[i] * _window[i];
      }
      final mags = _fft.realFft(frame).discardConjugates().magnitudes();
      final col = Float32List(maxBin);
      for (var b = 0; b < maxBin; b++) {
        col[b] = 20.0 * (math.log(mags[b] + 1e-9) / ln10);
      }
      onColumn(col);
      _acc.removeRange(0, hop);
    }
  }

  void reset() => _acc.clear();
}

double _db20(double m) => 20.0 * (math.log(m + 1e-9) / math.ln10);

/// Highest source sample rate the voice-metrics pass produces valid pitch
/// analysis for — the admission ceiling for file analysis
/// (`SpectrogramService.admitForAnalysis`).
///
/// Derivation: the YIN slice is one FFT frame of raw samples decimated by
/// yinFactor (≤ 2), so it always spans fftSize / sampleRate SECONDS —
/// 2048 samples at the production fftSize. [yinPitch]'s widened fmin search
/// lag is a fixed 1 / (70 Hz · 0.9) ≈ 1/63 s; once the frame is shorter than
/// that (sampleRate > 63 · 2048 ≈ 129 kHz) the lag is capped to the frame and
/// the integration window collapses below 2 samples, so EVERY frame returns
/// unvoiced regardless of content — F0, formants, H1−H2, register, vibrato
/// and the voice profile all silently empty while the (YIN-free) spectrogram
/// renders normally. Rates approaching the bound leave integration windows of
/// a handful of samples, i.e. garbage periodicity. 96 kHz is the highest
/// standard rate with a usable window, and no standard rate exists between it
/// and the dead zone, so the gate refuses anything above it instead of
/// producing a healthy-looking take with empty analyses.
const int maxAnalysisSampleRate = 96000;

/// YIN periodicity (1 − d′ at the chosen lag) below which a frame is treated as
/// unvoiced. Clearly-sung vowels read ≈ 0.8–0.95; room noise / breath read well
/// under this. Gating F0 *and* the formants on periodicity (not just loudness)
/// is what keeps formant dots off non-periodic noise that clears the RMS floor.
const double _voicedConfidence = 0.55;

/// YIN step 1, the squared-difference function d(τ) = Σⱼ (x[j] − x[j+τ])²
/// over the first [w] samples of [frame], for τ in 1..[tauMax] (d[0] is 0).
///
/// O(w·τmax) — ~245k multiply-adds per 48 kHz frame, the bulk of the take
/// analysis and of the live analyzer's UI-isolate budget. A one-lag loop is
/// bound by its single add chain, so the lags are swept FOUR at a time, each
/// with its own accumulator summed in the same j order: every d[τ] is the
/// same IEEE sum as the one-lag loop's, bit for bit (pinned by test), the
/// four independent chains just overlap. The Float64 copy is exact
/// (float32 → double is lossless) and spares four conversions per step.
@visibleForTesting
Float64List yinDifference(Float32List frame, int tauMax, int w) {
  final x = Float64List(frame.length);
  for (var i = 0; i < frame.length; i++) {
    x[i] = frame[i];
  }
  final d = Float64List(tauMax + 1);
  var tau = 1;
  for (; tau + 3 <= tauMax; tau += 4) {
    var s0 = 0.0, s1 = 0.0, s2 = 0.0, s3 = 0.0;
    for (var j = 0; j < w; j++) {
      final xj = x[j];
      final d0 = xj - x[j + tau];
      final d1 = xj - x[j + tau + 1];
      final d2 = xj - x[j + tau + 2];
      final d3 = xj - x[j + tau + 3];
      s0 += d0 * d0;
      s1 += d1 * d1;
      s2 += d2 * d2;
      s3 += d3 * d3;
    }
    d[tau] = s0;
    d[tau + 1] = s1;
    d[tau + 2] = s2;
    d[tau + 3] = s3;
  }
  for (; tau <= tauMax; tau++) {
    var sum = 0.0;
    for (var j = 0; j < w; j++) {
      final diff = x[j] - x[j + tau];
      sum += diff * diff;
    }
    d[tau] = sum;
  }
  return d;
}

/// YIN fundamental-frequency estimate (de Cheveigné & Kawahara, 2002) for one
/// time-domain [frame] sampled at [sampleRate]. Returns the pitch in Hz plus a
/// periodicity confidence in [0, 1] (1 = perfectly periodic). [f0] is NaN when
/// no periodic lag exists in [fmin]..[fmax].
///
/// Unlike the cepstrum/autocorrelation the live path used before, YIN works in
/// the time domain and the cumulative-mean normalization (step 2) suppresses the
/// zero-lag dip that makes those methods jump octaves — so it is both more
/// accurate and far less prone to octave errors. Pure + isolate-safe.
({double f0, double periodicity}) yinPitch(
  Float32List frame,
  double sampleRate, {
  double fmin = 70,
  double fmax = 1100,
  double threshold = 0.15,
}) {
  final n = frame.length;
  if (n < 4) return (f0: double.nan, periodicity: 0.0);
  // Search a little past the [fmin, fmax] band so a tone sitting on a boundary
  // produces an interior dip (with neighbours for parabolic interpolation)
  // instead of a half-period correlation pinned to the search edge — which
  // would otherwise report a false ~fmin pitch for sub-fmin tones and drop a
  // tone just under fmax. The final range gate still rejects out-of-band pitch.
  var tauMax = (sampleRate / (fmin * 0.9)).ceil();
  final tauMin = (sampleRate / (fmax * 1.1)).floor().clamp(2, n - 1);
  if (tauMax > n - 1) tauMax = n - 1;
  final w = n - tauMax; // integration window length
  if (w < 2 || tauMax <= tauMin) {
    return (f0: double.nan, periodicity: 0.0);
  }

  // 1) Squared-difference function d(τ) = Σⱼ (x[j] − x[j+τ])².
  final d = yinDifference(frame, tauMax, w);

  // 2) Cumulative mean normalized difference d′(τ) — d(τ) divided by the running
  // mean of d up to τ. d′(0)=1; this is what flattens the spurious low-τ dip.
  final dn = Float64List(tauMax + 1);
  dn[0] = 1.0;
  var running = 0.0;
  for (var tau = 1; tau <= tauMax; tau++) {
    running += d[tau];
    dn[tau] = running > 0 ? d[tau] * tau / running : 1.0;
  }

  // 3) Absolute threshold: the first lag whose dip falls below [threshold]
  // (descending to that dip's local minimum); if none does, fall back to the
  // in-band global minimum so periodicity still reflects the best lag. The
  // threshold scan starts below the band (d′ is defined from τ=1): starting it
  // at tauMin skipped an above-fmax tone's true period T and landed on its 2T
  // dip, which the range gate then accepted as f0/2 — an octave-low pitch at
  // full confidence. Found at T, such a tone fails the range gate → NaN.
  var tau = -1;
  for (var t = 2; t <= tauMax; t++) {
    if (dn[t] < threshold) {
      while (t + 1 <= tauMax && dn[t + 1] < dn[t]) {
        t++;
      }
      tau = t;
      break;
    }
  }
  if (tau == -1) {
    var best = tauMin;
    for (var t = tauMin + 1; t <= tauMax; t++) {
      if (dn[t] < dn[best]) best = t;
    }
    tau = best;
  } else if (sampleRate / _yinVertex(dn, tau, tauMax).lag > fmax * 1.03) {
    // The first dip lies ABOVE the band. For an above-band tone that is its
    // period, and the range gate below answers NaN, as it must. But a real
    // in-band note whose 2nd harmonic dominates — a bright high vowel, H2
    // 16 dB over H1/H3 — repeats almost exactly at T/2, so its first dip
    // sits there and the note read NaN (every such note above ~605 Hz once
    // the scan began below the band). It is only NEARLY periodic at T/2
    // (H1/H3 don't cancel there) and truly periodic at T: an in-band double
    // whose dip is clearly deeper, and whose fundamental is really in the
    // frame, is the period. An above-band tone is periodic at every multiple
    // and has nothing at f/2, so it still reads NaN.
    tau = _deeperInBandMultiple(frame, dn, tau, tauMin, tauMax) ?? tau;
  }

  // 4) Parabolic interpolation around the chosen lag for sub-sample precision,
  // and the depth of d′ AT that interpolated vertex (a touch below dn[tau]) —
  // which is the more faithful periodicity/voicing confidence.
  final vertex = _yinVertex(dn, tau, tauMax);
  final betterTau = vertex.lag;
  final dMin = vertex.depth;

  final periodicity = (1.0 - dMin).clamp(0.0, 1.0);
  final f0 = betterTau > 0 ? sampleRate / betterTau : double.nan;
  // A small tolerance keeps a tone sitting exactly on a band edge (whose
  // interpolated lag can land a hair outside) from being dropped, while a
  // genuinely out-of-band pitch (e.g. a sub-fmin bass note) still returns NaN.
  if (f0 < fmin * 0.97 || f0 > fmax * 1.03) {
    return (f0: double.nan, periodicity: periodicity);
  }
  return (f0: f0, periodicity: periodicity);
}

/// The parabolic vertex of d′ around integer lag [tau]: the sub-sample lag
/// and the interpolated depth there. Only a genuine (convex) minimum is
/// interpolated; otherwise the integer lag and its own depth.
({double lag, double depth}) _yinVertex(Float64List dn, int tau, int tauMax) {
  if (tau > 1 && tau < tauMax) {
    final s0 = dn[tau - 1], s1 = dn[tau], s2 = dn[tau + 1];
    final denom = s0 - 2 * s1 + s2;
    if (denom > 1e-12) {
      return (
        lag: tau + 0.5 * (s0 - s2) / denom,
        depth: s1 - (s0 - s2) * (s0 - s2) / (8 * denom),
      );
    }
  }
  return (lag: tau.toDouble(), depth: dn[tau]);
}

/// How much deeper (absolute d′) the in-band multiple's dip must be than an
/// above-band first dip to be taken as the true period. A note whose 2nd
/// harmonic sits 16 dB over H1/H3 dips ~0.1 at T/2 and ~0 at T.
const double _multipleMargin = 0.02;

/// The weakest fundamental, relative to the frame's overall level, that a
/// rescued period may claim (−35 dB). A note whose H2 dominates still has a
/// real H1 at that pitch (−16 dB in the case this exists for); an above-band
/// tone has nothing there but noise and window leakage (< −60 dB) — the
/// check that keeps a bright high tone, whose integer-lag misalignment can
/// leave its 2T dip deeper than its T dip, from reading an octave low.
const double _minFundamentalRatio = 0.0178;

/// The integer lag of the in-band double 2τ of the above-band dip at [tau]
/// when that is the period (see [yinPitch] step 3), or null. Only the double
/// is considered: a 3rd-harmonic-dominated note stays NaN, as before, rather
/// than widen what a misaligned high tone can be mistaken for.
int? _deeperInBandMultiple(
  Float32List frame,
  Float64List dn,
  int tau,
  int tauMin,
  int tauMax,
) {
  final first = _yinVertex(dn, tau, tauMax);
  final target = (first.lag * 2).round();
  if (target < tauMin || target >= tauMax) return null;
  // The double's own local minimum, within a lag of the target.
  var m = target;
  if (dn[target - 1] < dn[m]) m = target - 1;
  if (dn[target + 1] < dn[m]) m = target + 1;
  if (m < tauMin) return null;
  final twice = _yinVertex(dn, m, tauMax);
  if (twice.depth >= 0.5 * first.depth ||
      first.depth - twice.depth < _multipleMargin) {
    return null;
  }
  return _fundamentalRatio(frame, 1 / twice.lag) >= _minFundamentalRatio
      ? m
      : null;
}

/// The amplitude of the sinusoid at normalized frequency [cycles] (cycles
/// per sample) in [frame], relative to the frame's overall level — both
/// Hann-windowed, so a component of amplitude a among components a₁…aₖ reads
/// ≈ a / √(Σaᵢ²). One direct DFT term: O(n), on the rare rescue path only.
double _fundamentalRatio(Float32List frame, double cycles) {
  final n = frame.length;
  var re = 0.0, im = 0.0, energy = 0.0, wSum = 0.0;
  for (var i = 0; i < n; i++) {
    final w = 0.5 - 0.5 * math.cos(2 * math.pi * i / (n - 1));
    final x = frame[i] * w;
    final ph = 2 * math.pi * cycles * i;
    re += x * math.cos(ph);
    im += x * math.sin(ph);
    energy += frame[i] * frame[i] * w;
    wSum += w;
  }
  if (energy <= 0 || wSum <= 0) return 0;
  final amplitude = 2 * math.sqrt(re * re + im * im) / wSum;
  return amplitude / math.sqrt(2 * energy / wSum);
}

/// One live-analysis frame: the dB spectrogram column plus the per-frame voice
/// metrics (already trailing-median smoothed). Formant/F0 are NaN when unvoiced,
/// and [active] reflects the periodicity-based voicing decision (not loudness).
class LiveFrame {
  const LiveFrame(
    this.column,
    this.ring,
    this.tilt,
    this.cpp,
    this.f0,
    this.f1,
    this.f2,
    this.f3,
    this.active,
    this.f0Confidence,
    this.h1h2,
    this.loudness,
  );
  final Float32List column;
  final double ring, tilt, cpp, f0, f1, f2, f3;
  final bool active;

  /// YIN periodicity for this frame (0 when unvoiced) — drives the tuner lock.
  final double f0Confidence;

  /// H1−H2 (dB) for this frame, or NaN when unvoiced — drives the live register.
  final double h1h2;

  /// Frame RMS level (dBFS) — drives the live loudness / dynamics readout.
  final double loudness;
}

/// Real-time version of [computeVoiceMetrics]: feed mic samples, get one
/// [LiveFrame] per [hop] (dB column + ring/tilt/CPP/F0/formants). Same DSP as
/// the offline path, computed incrementally on the main thread (sub-ms/frame),
/// with trailing-median smoothing (no future frames available live).
class LiveVoiceAnalyzer {
  LiveVoiceAnalyzer({
    this.fftSize = 2048,
    this.hop = 512,
    this.sampleRate = 44100,
    this.maxHz = 5000,
  }) : maxBin = math.min(
         fftSize ~/ 2 + 1,
         (maxHz * fftSize / sampleRate).ceil() + 1,
       ),
       _window = Window.hanning(fftSize),
       _fft = FFT(fftSize),
       _half = fftSize ~/ 2,
       _binHz = sampleRate / fftSize,
       _dsFactor = (sampleRate / 11000).round().clamp(1, 8) {
    _dsRate = sampleRate / _dsFactor;
    _lpcOrder = (2 + _dsRate / 1000).round().clamp(8, 20);
    _lpcWin = fftSize ~/ _dsFactor;
    _lpcWindow = _hamming(_lpcWin);
    // F0 runs on a separate, only-lightly-decimated (~22 kHz) copy so the lag
    // resolution stays octave-robust at high pitch (see computeVoiceMetrics).
    _yinFactor = (sampleRate / 22050).round().clamp(1, 2);
    _yinRate = sampleRate / _yinFactor;
    int binOf(double hz) {
      final b = (hz / _binHz).round();
      return b < 0 ? 0 : (b > _half ? _half : b);
    }

    _b2k = binOf(2000);
    _b4k = binOf(4000);
    _b50 = binOf(50);
    _b1k = binOf(1000);
    _b5k = binOf(5000);
    _qMin = (sampleRate / 500).floor().clamp(2, _half);
    // Clamp to _half: only quefrencies in [0, _half] are independent in the symmetric
    // cepstrum (mirrored above Nyquist). Defensive — the live analyzer runs at 48 kHz
    // where qMax < _half anyway, but keeps the two analysis paths consistent.
    _qMax = (sampleRate / 60).ceil().clamp(_qMin + 1, _half);
  }

  final int fftSize, hop, sampleRate;
  final double maxHz;
  final int maxBin;
  final Float64List _window;
  final FFT _fft;
  final int _half;
  final double _binHz;
  final int _dsFactor;
  late final double _dsRate;
  late final int _yinFactor;
  late final double _yinRate;
  late final int _lpcOrder, _lpcWin;
  late final Float64List _lpcWindow;
  late final int _b2k, _b4k, _b50, _b1k, _b5k, _qMin, _qMax;

  final List<double> _acc = [];
  final List<double> _f0H = [], _f1H = [], _f2H = [], _f3H = [], _h1h2H = [];
  static const int _smoothN = 5;
  // Absolute RMS floor (dBFS) below which a frame is treated as silence/noise —
  // room noise is typically < −50 dBFS, sung/spoken voice > −35.
  static const double _vadFloorDb = -47;
  // Overlay sustain gate: F0/formant dots (and the spectrogram brightness) only
  // appear once the voice has been continuously voiced for [_overlayAttack]
  // frames, and survive up to [_overlayRelease] unvoiced frames before resetting
  // — so brief/scattered voiced blips (speech, breath, taps) never draw dots,
  // only sustained singing does (mirrors how the tuner waits for a held note).
  static const int _overlayAttack = 4;
  static const int _overlayRelease = 4;
  int _voicedRun = 0, _overlaySilent = 0;

  double get binHz => _binHz;
  int get bins => maxBin;
  double get frameRate => sampleRate / hop;

  /// Appends [samples] and emits a [LiveFrame] via [onFrame] for every [hop].
  void addSamples(Float32List samples, void Function(LiveFrame f) onFrame) {
    _acc.addAll(samples);
    final raw = Float32List(fftSize);
    final frame = Float64List(fftSize);
    final logmag = Float64List(fftSize);
    while (_acc.length >= fftSize) {
      var rawSq = 0.0;
      for (var i = 0; i < fftSize; i++) {
        raw[i] = _acc[i];
        rawSq += raw[i] * raw[i];
        frame[i] = raw[i] * _window[i];
      }
      final mags = _fft.realFft(frame).discardConjugates().magnitudes();

      final col = Float32List(maxBin);
      for (var b = 0; b < maxBin; b++) {
        col[b] = _db20(mags[b]);
      }
      // Voice-activity floor: reject anything below an absolute RMS level so the
      // analyzer never reacts to room noise; the YIN periodicity gate (below)
      // then separates a real voice from non-periodic noise above the floor.
      final rmsDb = _db20(math.sqrt(rawSq / fftSize));
      final audible = rmsDb > _vadFloorDb;

      // Ring + tilt. Skip the DC bin in the low-band peak search.
      var lowPk = 1e-12, highPk = 1e-12;
      for (var b = 1; b < _b2k; b++) {
        if (mags[b] > lowPk) lowPk = mags[b];
      }
      for (var b = _b2k; b < _b4k; b++) {
        if (mags[b] > highPk) highPk = mags[b];
      }
      final ring = _db20(highPk) - _db20(lowPk);
      var lowE = 0.0, highE = 0.0;
      for (var b = _b50; b < _b1k; b++) {
        lowE += mags[b] * mags[b];
      }
      for (var b = _b1k; b < _b5k; b++) {
        highE += mags[b] * mags[b];
      }
      final tilt =
          10.0 * (math.log((highE + 1e-12) / (lowE + 1e-12)) / math.ln10);

      // Cepstrum → CPP + F0.
      for (var k = 0; k <= _half; k++) {
        logmag[k] = _db20(mags[k]);
      }
      for (var k = _half + 1; k < fftSize; k++) {
        logmag[k] = logmag[fftSize - k];
      }
      final cep = _fft.realFft(logmag);
      var peakQ = _qMin, peakV = -1e30;
      for (var q = _qMin; q <= _qMax; q++) {
        final v = cep[q].x / fftSize;
        if (v > peakV) {
          peakV = v;
          peakQ = q;
        }
      }
      // Baseline regression excludes a small neighborhood of the peak (see
      // computeVoiceMetrics) so the prominence isn't biased down.
      const cppGuard = 3;
      var sx = 0.0, sy = 0.0, sxx = 0.0, sxy = 0.0, cnt = 0;
      for (var q = _qMin; q <= _qMax; q++) {
        if ((q - peakQ).abs() <= cppGuard) continue;
        final v = cep[q].x / fftSize;
        final qd = q.toDouble();
        sx += qd;
        sy += v;
        sxx += qd * qd;
        sxy += qd * v;
        cnt++;
      }
      final cntD = cnt.toDouble();
      final den = cntD * sxx - sx * sx;
      final slope = den.abs() < 1e-12 ? 0.0 : (cntD * sxy - sx * sy) / den;
      final intercept = cnt == 0 ? 0.0 : (sy - slope * sx) / cntD;
      final cpp = peakV - (slope * peakQ.toDouble() + intercept);

      // F0 + voicing via YIN on a decimated copy of this window — but only when
      // the frame is audible, so a silent room skips the decimate + YIN cost
      // every hop (voiced requires audible anyway, so the result is unchanged).
      Float32List? ds;
      var yinF0 = double.nan, periodicity = 0.0;
      if (audible) {
        ds = _decimate(raw, _dsFactor, sampleRate.toDouble());
        // YIN on the lighter ~22 kHz decimation; formants stay on the 11 kHz ds.
        final yinSig = _yinFactor == 1
            ? raw
            : _decimate(raw, _yinFactor, sampleRate.toDouble());
        final yin = yinPitch(yinSig, _yinRate, fmin: 70, fmax: 1100);
        yinF0 = yin.f0;
        periodicity = yin.periodicity;
      }
      final voiced = audible && periodicity >= _voicedConfidence;
      // A frame can be voiced (periodic) yet have no in-band F0 (pitch outside
      // 70–1100 Hz → NaN). Gate the F0 channel separately so the tuner never
      // locks a stale value and no stale pitch dot is drawn for that frame.
      final f0Voiced = voiced && !yinF0.isNaN;

      // Sustain gate: count consecutive voiced frames (tolerating short unvoiced
      // dropouts) and only treat the frame as 'sustained' once the run clears
      // the attack — so only held notes drive the overlay/spectrogram, not the
      // brief voiced blips of speech/breath/background.
      if (voiced) {
        _voicedRun++;
        _overlaySilent = 0;
      } else if (++_overlaySilent > _overlayRelease) {
        _voicedRun = 0;
      }
      final sustained = _voicedRun >= _overlayAttack;

      // Formants (LPC on the same decimated window) only on voiced frames —
      // formants are a property of phonation, not of noise.
      var rf1 = double.nan, rf2 = double.nan, rf3 = double.nan;
      if (voiced) {
        final cands = _lpcFormants(
          ds!,
          _dsRate,
          _lpcOrder,
          _lpcWin,
          _lpcWindow,
          0.97,
          ds.length ~/ 2,
        );
        final (af1, af2, af3) = _assignFormants(cands);
        rf1 = af1;
        rf2 = af2;
        rf3 = af3;
      }

      // H1−H2 register cue (only on a sustained, in-band voiced frame), median-
      // smoothed like the other live channels so the register readout is steady
      // (raw H1−H2 is noisy frame-to-frame and would jitter across the
      // chest/mix/head score thresholds).
      var rawH1h2 = double.nan;
      final h1h2Voiced =
          sustained && f0Voiced && yinF0 > 0 && 2 * yinF0 < sampleRate / 2;
      if (h1h2Voiced) {
        final hw = ((yinF0 / _binHz - 1) / 2).floor().clamp(0, 2);
        rawH1h2 =
            _db20(_harmonicPeak(mags, yinF0 / _binHz, hw)) -
            _db20(_harmonicPeak(mags, 2 * yinF0 / _binHz, hw));
      }
      final h1h2 = _smoothGated(_h1h2H, rawH1h2, h1h2Voiced);

      onFrame(
        LiveFrame(
          col,
          ring,
          tilt,
          cpp,
          _smoothGated(_f0H, yinF0, sustained && f0Voiced),
          _smoothGated(_f1H, rf1, sustained && voiced),
          _smoothGated(_f2H, rf2, sustained && voiced),
          _smoothGated(_f3H, rf3, sustained && voiced),
          sustained,
          f0Voiced ? periodicity : 0.0,
          h1h2,
          rmsDb,
        ),
      );
      _acc.removeRange(0, hop);
    }
  }

  /// Trailing median (last [_smoothN] frames), ignoring NaN — but only emits a
  /// value on voiced frames. On unvoiced frames it records the gap (NaN) and
  /// returns NaN, so a recent voiced value is never smeared onto silence/noise.
  double _smoothGated(List<double> hist, double v, bool voiced) {
    hist.add(voiced ? v : double.nan);
    if (hist.length > _smoothN) hist.removeAt(0);
    if (!voiced) return double.nan;
    final valid = [
      for (final x in hist)
        if (!x.isNaN) x,
    ]..sort();
    return valid.isEmpty ? double.nan : valid[valid.length ~/ 2];
  }

  void reset() {
    _acc.clear();
    _f0H.clear();
    _f1H.clear();
    _f2H.clear();
    _f3H.clear();
    _h1h2H.clear();
    _voicedRun = 0;
    _overlaySilent = 0;
  }
}

/// Stabilizes a noisy live F0 stream into a steady tuner reading. A note must be
/// clearly voiced (periodicity ≥ [minConfidence]) and hold for [attack]
/// consecutive frames within ~1.5 semitones before it shows — which suppresses
/// the silence/onset jitter — then it EMA-glides; it releases back to NaN after
/// [release] unvoiced frames. Pure + deterministic (unit-testable).
class TunerTracker {
  TunerTracker({
    this.minConfidence = 0.45,
    this.attack = 5,
    this.release = 6,
    this.alpha = 0.13,
  });

  final double minConfidence;
  final int attack;
  final int release;
  final double alpha;

  double _ema = double.nan;
  int _hold = 0, _silent = 0;

  /// Feeds one frame's [f0] + its YIN periodicity [confidence]; returns the
  /// stable tuner pitch (Hz) or NaN when nothing is locked.
  double add(double f0, double confidence) {
    final voiced = !f0.isNaN && f0 > 0 && confidence >= minConfidence;
    if (voiced) {
      if (_ema.isNaN) {
        _ema = f0;
        _hold = 1;
      } else {
        final semis = (12 * (math.log(f0 / _ema) / math.ln2)).abs();
        if (semis < 1.5) {
          _ema = _ema * (1 - alpha) + f0 * alpha;
          _hold++;
        } else {
          _ema = f0; // jumped to a new note — re-lock (re-attack)
          _hold = 1;
        }
      }
      _silent = 0;
    } else if (++_silent > release) {
      _ema = double.nan;
      _hold = 0;
    }
    return _hold >= attack ? _ema : double.nan;
  }

  void reset() {
    _ema = double.nan;
    _hold = 0;
    _silent = 0;
  }
}
