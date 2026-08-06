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

/// Merges several clips' [Spectrogram]s into one lane-spanning analysis for a
/// multi-clip take. Each part carries [offsetMs] — the clip's start relative to
/// the LANE's span start (clip.startMs − track.startMs, so ≥ 0) — and its
/// frames are placed at that offset, so frame f of the result always sits
/// f·hop/sampleRate seconds after the lane start. Gaps between clips become
/// floor (merged minDb) columns, which the heat renderer draws as silence;
/// where clips overlap, the later-starting clip wins.
///
/// Parts must share [bins], [fftSize], [sampleRate] and [hop]; any part whose
/// framing differs from the first is skipped, as are frameless parts. Returns
/// null when nothing survives, and a lone survivor starting at the lane origin
/// unchanged. Pure.
Spectrogram? mergeSpectrograms(
  List<({int offsetMs, Spectrogram spectrogram})> parts,
) {
  assert(
    parts.every((p) => p.offsetMs >= 0),
    'offsets are lane-relative and must be >= 0',
  );
  if (parts.isEmpty) return null;
  final first = parts.first.spectrogram;
  // Skip frameless parts too: they carry nothing to place, and must not
  // stretch the output span with pure gap.
  final kept = parts
      .where(
        (p) =>
            p.spectrogram.frames > 0 &&
            p.spectrogram.bins == first.bins &&
            p.spectrogram.fftSize == first.fftSize &&
            p.spectrogram.sampleRate == first.sampleRate &&
            p.spectrogram.hop == first.hop,
      )
      .toList();
  if (kept.isEmpty) return null;
  int frameOf(int offsetMs) =>
      (offsetMs / 1000 * first.sampleRate / first.hop).round();
  // A lone part at the lane origin IS the whole lane's analysis — unchanged.
  if (kept.length == 1 && frameOf(kept.single.offsetMs) == 0) {
    return kept.single.spectrogram;
  }

  // Place parts in ascending start order (stable on ties), so a later-starting
  // clip's frames overwrite an earlier clip's where they overlap.
  final order = [for (var i = 0; i < kept.length; i++) i]
    ..sort((a, b) {
      final byStart = kept[a].offsetMs.compareTo(kept[b].offsetMs);
      return byStart != 0 ? byStart : a.compareTo(b);
    });

  final bins = first.bins;
  var total = 0;
  var minDb = double.infinity;
  var maxDb = -double.infinity;
  for (final p in kept) {
    final end = frameOf(p.offsetMs) + p.spectrogram.frames;
    if (end > total) total = end;
    if (p.spectrogram.minDb < minDb) minDb = p.spectrogram.minDb;
    if (p.spectrogram.maxDb > maxDb) maxDb = p.spectrogram.maxDb;
  }
  // The merged array is sized by the lane SPAN, not the audio in it: two
  // short clips hours apart would allocate gigabytes of gap columns. Cap the
  // span-derived allocation and fail soft (no merged view) beyond it — at
  // ~214 bins that admits roughly 50 minutes of lane, far past any legal
  // per-clip analysis (10 min) while refusing corrupt/hostile offsets.
  const maxMergedCells = 64 * 1000 * 1000; // Float32 cells ≈ 256 MB
  if (total * bins > maxMergedCells) return null;
  final floorDb = minDb.isFinite ? minDb : 0.0;

  // Pre-fill with the merged floor so gap columns read as silence (the darkest
  // level any clip carries) instead of the Float32List default of 0 dB, which
  // the auto-scaled heat map would render as bright noise.
  final db = Float32List(total * bins)..fillRange(0, total * bins, floorDb);
  for (final i in order) {
    final p = kept[i];
    final at = frameOf(p.offsetMs) * bins;
    db.setRange(at, at + p.spectrogram.frames * bins, p.spectrogram.db);
  }
  return Spectrogram(
    frames: total,
    bins: bins,
    maxHz: first.maxHz,
    sampleRate: first.sampleRate,
    fftSize: first.fftSize,
    hop: first.hop,
    db: db,
    minDb: floorDb,
    maxDb: maxDb.isFinite ? maxDb : 0,
  );
}
