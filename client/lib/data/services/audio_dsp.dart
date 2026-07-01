import 'dart:math' as math;
import 'dart:typed_data';

import 'package:fftea/fftea.dart';

import '../../domain/models/spectrogram.dart';
import '../../domain/models/voice_metrics.dart';

/// Decoded mono PCM plus its sample rate.
class PcmAudio {
  PcmAudio(this.samples, this.sampleRate);
  final Float64List samples;
  final int sampleRate;
}

/// Encodes mono [samples] (in [-1, 1]) as a 16-bit PCM WAV. Isolate-safe.
Uint8List writeWavMono(Float64List samples, int sampleRate) {
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

/// Parses a 16-bit PCM WAV into mono samples in [-1, 1]. Returns null if the
/// bytes aren't uncompressed PCM16 (e.g. compressed m4a/mp3). Isolate-safe.
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

  if (dataOffset < 0 ||
      format != 1 ||
      bits != 16 ||
      channels < 1 ||
      sampleRate <= 0) {
    return null;
  }

  final total = math.min(dataLen, bytes.length - dataOffset) ~/ 2;
  final frames = total ~/ channels;
  final out = Float64List(frames);
  var p = dataOffset;
  for (var i = 0; i < frames; i++) {
    var sum = 0.0;
    for (var c = 0; c < channels; c++) {
      sum += bd.getInt16(p, Endian.little) / 32768.0;
      p += 2;
    }
    out[i] = sum / channels;
  }
  return PcmAudio(out, sampleRate);
}

/// Computes a [Spectrogram] from mono [samples]. Pure + isolate-safe.
Spectrogram computeSpectrogram(
  Float64List samples,
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
    maxHz: maxHz,
    sampleRate: sampleRate,
    fftSize: fftSize,
    hop: hop,
    db: db,
    minDb: minDb,
    maxDb: maxDb,
  );
}

/// Computes per-frame voice-timbre metrics from mono [samples] — ring/SPR (F18),
/// spectral tilt / alpha ratio (F19), cepstral peak prominence (F20) and the
/// first three formants F1/F2/F3 (F17). Same framing as [computeSpectrogram].
/// Pure + isolate-safe. See [VoiceMetrics] for the meaning of each curve.
VoiceMetrics computeVoiceMetrics(
  Float64List samples,
  int sampleRate, {
  int fftSize = 2048,
  int hop = 512,
}) {
  const ln10 = 2.302585092994046;
  double db(double mag) => 20.0 * (math.log(mag + 1e-9) / ln10);

  final binHz = sampleRate / fftSize;
  final half = fftSize ~/ 2;
  int binOf(double hz) {
    var b = (hz / binHz).round();
    if (b < 0) b = 0;
    if (b > half) b = half;
    return b;
  }

  final window = Window.hanning(fftSize);
  final fft = FFT(fftSize);

  // Match fftea's STFT framing exactly (computeSpectrogram uses STFT.run, which
  // zero-pads and emits a trailing partial frame) so VoiceMetrics.frames ==
  // Spectrogram.frames and the per-frame overlay/playhead stay aligned. The
  // final frame is zero-padded past the end of [samples].
  final len = samples.length;
  final nFrames = len == 0
      ? 0
      : (len <= fftSize ? 1 : ((len - fftSize + hop - 1) ~/ hop) + 1);

  final ring = Float32List(nFrames);
  final tilt = Float32List(nFrames);
  final cpp = Float32List(nFrames);
  final loudness = Float32List(nFrames);
  final f0 = Float32List(nFrames);
  final f1 = Float32List(nFrames);
  final f2 = Float32List(nFrames);
  final f3 = Float32List(nFrames);
  final h1h2 = Float32List(nFrames);
  final active = Uint8List(nFrames);
  final energy = Float64List(nFrames);
  final rf0 = Float32List(nFrames);
  final rh1h2 = Float32List(nFrames); // raw H1−H2, median-smoothed into h1h2
  final per = Float32List(nFrames); // YIN periodicity per frame (voicing gate)

  // Band edges (bins).
  final b2k = binOf(2000), b4k = binOf(4000);
  final b50 = binOf(50), b1k = binOf(1000), b5k = binOf(5000);
  // CPP quefrency search window (F0 ≈ 60..500 Hz).
  final qMin = (sampleRate / 500).floor().clamp(2, half);
  // Clamp to `half`: the cepstrum is symmetric (logmag is mirrored above the Nyquist bin
  // below), so only quefrencies q in [0, half] are independent. The old `fftSize - 1`
  // upper bound let the search read mirrored (aliased) quefrencies at sample rates above
  // ~62 kHz, corrupting CPP for low pitches.
  final qMax = (sampleRate / 60).ceil().clamp(qMin + 1, half);
  // F17 formants use LPC on a band-limited (decimated to ~11 kHz) copy of the
  // signal — far steadier than spectral peak-picking. Raw per-frame estimates
  // go to rf1/rf2/rf3 and are median-smoothed into f1/f2/f3 at the end.
  final dsFactor = (sampleRate / 11000).round().clamp(1, 8);
  final dsRate = sampleRate / dsFactor;
  final ds = _decimate(samples, dsFactor, sampleRate.toDouble());
  final lpcOrder = (2 + dsRate / 1000).round().clamp(8, 20);
  final lpcWin = fftSize ~/ dsFactor;
  final lpcWindow = _hamming(lpcWin);
  const preCoef = 0.97;
  final rf1 = Float32List(nFrames);
  final rf2 = Float32List(nFrames);
  final rf3 = Float32List(nFrames);

  // F0 runs on an only-lightly-decimated copy (~22 kHz) — NOT the 11 kHz formant
  // decimation. YIN is a time-domain lag estimator, so its precision is set by
  // the sample rate; at 11 kHz the integer-lag spacing is ~1.5 semitones near
  // the top of the range (1100 Hz ≈ 10-sample period), which causes octave/
  // subharmonic locks under noise. ~22 kHz keeps the lag fine enough to stay
  // octave-robust across 70–1100 Hz at a fraction of full-rate cost.
  final yinFactor = (sampleRate / 22050).round().clamp(1, 2);
  final yinRate = sampleRate / yinFactor;
  final yinSig = yinFactor == 1
      ? samples
      : _decimate(samples, yinFactor, sampleRate.toDouble());
  final yinWin = (fftSize / yinFactor).round();

  final frame = Float64List(fftSize);
  final logmag = Float64List(fftSize);
  var maxEnergy = 1e-12;

  for (var fr = 0; fr < nFrames; fr++) {
    final start = fr * hop;

    // --- Plain windowed magnitude spectrum (F18 / F19 / energy / CPP) +
    // the raw-RMS loudness (F9). ---
    var rawSq = 0.0;
    for (var i = 0; i < fftSize; i++) {
      final s = start + i < len ? samples[start + i] : 0.0;
      rawSq += s * s;
      frame[i] = s * window[i];
    }
    loudness[fr] = 20.0 * (math.log(math.sqrt(rawSq / fftSize) + 1e-9) / ln10);
    final mags = fft.realFft(frame).discardConjugates().magnitudes();

    var e = 0.0;
    for (var b = 1; b <= half; b++) {
      e += mags[b] * mags[b];
    }
    energy[fr] = e;
    if (e > maxEnergy) maxEnergy = e;

    // F18 — ring / Singing Power Ratio. Skip the DC bin so a decode offset can
    // never be picked as the low-band peak.
    var lowPk = 1e-12, highPk = 1e-12;
    for (var b = 1; b < b2k; b++) {
      if (mags[b] > lowPk) lowPk = mags[b];
    }
    for (var b = b2k; b < b4k; b++) {
      if (mags[b] > highPk) highPk = mags[b];
    }
    ring[fr] = db(highPk) - db(lowPk);

    // F19 — spectral tilt / alpha ratio.
    var lowE = 0.0, highE = 0.0;
    for (var b = b50; b < b1k; b++) {
      lowE += mags[b] * mags[b];
    }
    for (var b = b1k; b < b5k; b++) {
      highE += mags[b] * mags[b];
    }
    tilt[fr] = 10.0 * (math.log((highE + 1e-12) / (lowE + 1e-12)) / ln10);

    // F20 — CPP: real cepstrum of the dB spectrum, peak prominence above its
    // least-squares regression over the F0 quefrency window.
    for (var k = 0; k <= half; k++) {
      logmag[k] = db(mags[k]);
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
    final yin = _yinWindow(
      yinSig,
      yinRate,
      ((fr * hop + fftSize / 2) / yinFactor).round(),
      yinWin,
    );
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
          db(_harmonicPeak(mags, f0Hz / binHz, hw)) -
          db(_harmonicPeak(mags, 2 * f0Hz / binHz, hw));
    } else {
      rh1h2[fr] = double.nan;
    }

    // F17 — formants via LPC on the decimated frame centered at this time, then
    // assign poles to the F1/F2/F3 bands (stable ordinal labels).
    final cands = _lpcFormants(
      ds,
      dsRate,
      lpcOrder,
      lpcWin,
      lpcWindow,
      preCoef,
      ((fr * hop + fftSize / 2) / dsFactor).round(),
    );
    final (af1, af2, af3) = _assignFormants(cands);
    rf1[fr] = af1;
    rf2[fr] = af2;
    rf3[fr] = af3;
  }

  // Voicing gate: a frame is voiced only when it is both audible (within 30 dB
  // of peak energy) AND periodic (YIN periodicity ≥ _voicedConfidence). Averages
  // use audible frames; F0/formants are nulled on every non-voiced frame so the
  // overlay never draws pitch/formant dots on noise or breath.
  final gate = maxEnergy * 1e-3;
  var rSum = 0.0, tSum = 0.0, cSum = 0.0, n = 0;
  for (var fr = 0; fr < nFrames; fr++) {
    final audible = energy[fr] >= gate;
    final voiced = audible && per[fr] >= _voicedConfidence;
    // [active] tracks audible frames (LTAS), but the ring/tilt/CPP timbre
    // averages use only VOICED frames — averaging over breath/consonant noise
    // dilutes them and mismatched the live readout's voiced-only average.
    if (audible) active[fr] = 1;
    if (voiced) {
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
  final inv = n == 0 ? 0.0 : 1.0 / n;

  // --- Input quality: peak level, clipping, SNR ----------------------------
  // True clipping is a near-full-scale run whose samples are PINNED (flat top):
  // a clean loud tone curves through its peak, so its samples differ frame to
  // frame; a clipped one holds the same ceiling value. Requiring near-flatness
  // (Δ ≤ flatEps) over a run ≥ clipRun avoids false-positiving a hot low-F0 tone.
  var peak = 0.0, clipped = 0, run = 0;
  var prevHigh = 0.0;
  const clipLevel = 0.997, clipRun = 4, flatEps = 0.0003;
  for (var i = 0; i < len; i++) {
    final a = samples[i].abs();
    if (a > peak) peak = a;
    if (a >= clipLevel && (run == 0 || (a - prevHigh).abs() <= flatEps)) {
      run++;
      prevHigh = a;
      if (run == clipRun) {
        clipped += clipRun;
      } else if (run > clipRun) {
        clipped++;
      }
    } else {
      run = 0;
    }
  }
  final peakDbfs = len == 0
      ? double.nan
      : 20.0 * (math.log(peak + 1e-9) / ln10);
  final clippedFraction = len == 0 ? 0.0 : clipped / len;
  // SNR: median loudness of VOICED frames over the noise floor (the 10th-pct of
  // all frame loudness ≈ background between phrases).
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

/// One 2nd-order Butterworth low-pass (RBJ biquad, Q≈0.707).
Float64List _biquadLp(Float64List x, double fc, double fs) {
  final w0 = 2 * math.pi * fc / fs;
  final cosw = math.cos(w0), sinw = math.sin(w0);
  final alpha = sinw / math.sqrt2;
  final a0 = 1 + alpha;
  final b0 = (1 - cosw) / 2 / a0;
  final b1 = (1 - cosw) / a0;
  final b2 = b0;
  final a1 = -2 * cosw / a0;
  final a2 = (1 - alpha) / a0;
  final out = Float64List(x.length);
  var x1 = 0.0, x2 = 0.0, y1 = 0.0, y2 = 0.0;
  for (var i = 0; i < x.length; i++) {
    final xi = x[i];
    final yi = b0 * xi + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
    out[i] = yi;
    x2 = x1;
    x1 = xi;
    y2 = y1;
    y1 = yi;
  }
  return out;
}

/// Anti-aliased decimation by integer [factor] (two cascaded Butterworth LPs).
Float64List _decimate(Float64List x, int factor, double fs) {
  if (factor <= 1) return x;
  // Anti-alias below the new Nyquist. Three cascaded biquads (~6th order) give
  // markedly more stopband attenuation than two, so audible energy above the
  // fold doesn't alias into the formant bands. fc = 0.45·newSampleRate, i.e.
  // 0.9·newNyquist, so F3 (≤ ~3.8 kHz) is still well inside the passband.
  final fc = 0.45 * fs / factor;
  final lp = _biquadLp(_biquadLp(_biquadLp(x, fc, fs), fc, fs), fc, fs);
  final m = x.length ~/ factor;
  final out = Float64List(m);
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
  Float64List ds,
  double dsRate,
  int order,
  int win,
  Float64List window,
  double preCoef,
  int center,
) {
  if (ds.length < 2) return const [];
  final start = center - win ~/ 2;
  final frame = Float64List(win);
  var prev = (start - 1 >= 0 && start - 1 < ds.length) ? ds[start - 1] : 0.0;
  var energy = 0.0;
  for (var i = 0; i < win; i++) {
    final idx = start + i;
    final s = (idx >= 0 && idx < ds.length) ? ds[idx] : 0.0;
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
void _medianInto(Float32List src, Float32List dst, int half) {
  final n = src.length;
  final buf = <double>[];
  for (var i = 0; i < n; i++) {
    // A NaN center means this frame was gated out (unvoiced): keep it NaN so the
    // smoothing never borrows neighbours' F0/formants onto breath/silence.
    if (src[i].isNaN) {
      dst[i] = double.nan;
      continue;
    }
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
    Float64List samples,
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

/// YIN periodicity (1 − d′ at the chosen lag) below which a frame is treated as
/// unvoiced. Clearly-sung vowels read ≈ 0.8–0.95; room noise / breath read well
/// under this. Gating F0 *and* the formants on periodicity (not just loudness)
/// is what keeps formant dots off non-periodic noise that clears the RMS floor.
const double _voicedConfidence = 0.55;

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
  Float64List frame,
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
  final d = Float64List(tauMax + 1);
  for (var tau = 1; tau <= tauMax; tau++) {
    var sum = 0.0;
    for (var j = 0; j < w; j++) {
      final diff = frame[j] - frame[j + tau];
      sum += diff * diff;
    }
    d[tau] = sum;
  }

  // 2) Cumulative mean normalized difference d′(τ) — d(τ) divided by the running
  // mean of d up to τ. d′(0)=1; this is what flattens the spurious low-τ dip.
  final dn = Float64List(tauMax + 1);
  dn[0] = 1.0;
  var running = 0.0;
  for (var tau = 1; tau <= tauMax; tau++) {
    running += d[tau];
    dn[tau] = running > 0 ? d[tau] * tau / running : 1.0;
  }

  // 3) Absolute threshold: the first lag in the search band whose dip falls
  // below [threshold] (descending to that dip's local minimum); if none does,
  // fall back to the global minimum so periodicity still reflects the best lag.
  var tau = -1;
  for (var t = tauMin; t <= tauMax; t++) {
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
  }

  // 4) Parabolic interpolation around the chosen lag for sub-sample precision,
  // and the depth of d′ AT that interpolated vertex (a touch below dn[tau]) —
  // which is the more faithful periodicity/voicing confidence.
  var betterTau = tau.toDouble();
  var dMin = dn[tau];
  if (tau > 1 && tau < tauMax) {
    final s0 = dn[tau - 1], s1 = dn[tau], s2 = dn[tau + 1];
    final denom = s0 - 2 * s1 + s2;
    if (denom > 1e-12) {
      // Only interpolate a genuine (convex) minimum.
      betterTau = tau + 0.5 * (s0 - s2) / denom;
      dMin = s1 - (s0 - s2) * (s0 - s2) / (8 * denom);
    }
  }

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

/// Extracts a [win]-sample frame from decimated signal [ds] centered at sample
/// [center] (zero-padded at the edges) and runs [yinPitch] on it at [rate].
({double f0, double periodicity}) _yinWindow(
  Float64List ds,
  double rate,
  int center,
  int win,
) {
  final frame = Float64List(win);
  final start = center - win ~/ 2;
  for (var i = 0; i < win; i++) {
    final idx = start + i;
    frame[i] = (idx >= 0 && idx < ds.length) ? ds[idx] : 0.0;
  }
  return yinPitch(frame, rate);
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
  void addSamples(Float64List samples, void Function(LiveFrame f) onFrame) {
    _acc.addAll(samples);
    final raw = Float64List(fftSize);
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
      Float64List? ds;
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
