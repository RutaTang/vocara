import 'dart:typed_data';

/// A computed spectrogram: a [frames] × [bins] grid of magnitudes in dB,
/// covering 0..[maxHz]. Row-major: `db[frame * bins + bin]`. Frequency is
/// linear in bin index (bin b ≈ b * sampleRate / fftSize Hz).
class Spectrogram {
  Spectrogram({
    required this.frames,
    required this.bins,
    required this.maxHz,
    required this.sampleRate,
    required this.fftSize,
    required this.hop,
    required this.db,
    required this.minDb,
    required this.maxDb,
  });

  final int frames;
  final int bins;
  final double maxHz;
  final int sampleRate;
  final int fftSize;
  final int hop;

  /// Magnitudes in dB, row-major (`frame * bins + bin`).
  final Float32List db;

  final double minDb;
  final double maxDb;

  double freqOfBin(int bin) => bin * sampleRate / fftSize;
  double get binHz => sampleRate / fftSize;
  double timeOfFrame(int frame) => frame * hop / sampleRate;
  double get durationSeconds => frames * hop / sampleRate;

  /// dB value at a frame/bin (clamped to range).
  double valueAt(int frame, int bin) => db[frame * bins + bin];

  /// The column (dB per bin) nearest [seconds]; empty if no frames.
  Float32List columnAtTime(double seconds) {
    if (frames == 0) return Float32List(0);
    var frame = (seconds * sampleRate / hop).round();
    if (frame < 0) frame = 0;
    if (frame >= frames) frame = frames - 1;
    return Float32List.sublistView(db, frame * bins, frame * bins + bins);
  }
}

/// Concatenates several takes' [Spectrogram]s (passed in timeline order) into
/// one, for whole-lane analysis across a multi-clip take. Parts must share
/// [bins], [fftSize], [sampleRate] and [hop]; any part whose framing differs
/// from the first is skipped. Returns null for an empty list and the single part
/// unchanged for one. Pure.
Spectrogram? mergeSpectrograms(List<Spectrogram> parts) {
  if (parts.isEmpty) return null;
  if (parts.length == 1) return parts.first;
  final first = parts.first;
  final kept = parts
      .where(
        (p) =>
            p.bins == first.bins &&
            p.fftSize == first.fftSize &&
            p.sampleRate == first.sampleRate &&
            p.hop == first.hop,
      )
      .toList();
  if (kept.isEmpty) return null;
  if (kept.length == 1) return kept.first;

  final bins = first.bins;
  final total = kept.fold(0, (n, p) => n + p.frames);
  final db = Float32List(total * bins);
  var minDb = double.infinity;
  var maxDb = -double.infinity;
  var off = 0;
  for (final p in kept) {
    db.setRange(off, off + p.frames * bins, p.db);
    if (p.minDb < minDb) minDb = p.minDb;
    if (p.maxDb > maxDb) maxDb = p.maxDb;
    off += p.frames * bins;
  }
  return Spectrogram(
    frames: total,
    bins: bins,
    maxHz: first.maxHz,
    sampleRate: first.sampleRate,
    fftSize: first.fftSize,
    hop: first.hop,
    db: db,
    minDb: minDb.isFinite ? minDb : 0,
    maxDb: maxDb.isFinite ? maxDb : 0,
  );
}
