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

/// The frame a part starting [offsetMs] after the lane's span start occupies
/// on the merged grid, for a [sampleRate]/[hop] framing. The spectrogram and
/// voice-metrics merges of one take must round this identically: their outputs
/// are read against a single lane axis, so a one-frame disagreement slides the
/// pitch/formant overlays off the columns they annotate.
int mergeFrameOf(int offsetMs, int sampleRate, int hop) =>
    (offsetMs / 1000 * sampleRate / hop).round();

/// The longest lane span a merged analysis may materialize, in frames. A merge
/// is sized by the lane SPAN, not by the audio in it, so two short clips placed
/// hours apart — or an offset that arrived corrupt from persistence or a sync
/// peer — would allocate the whole gap. At 48 kHz with hop 512 this admits
/// 53 minutes of lane, far past the 10-minute per-clip analysis ceiling that
/// bounds any legal take, and costs ~11 MB of voice-metrics curves or ~257 MB
/// of 214-bin spectrogram.
const int maxMergedLaneFrames = 300 * 1000;

/// Whether a merged lane span of [frames] frames may be materialized. BOTH
/// lane merges ask before they allocate, so an illegal span cannot be admitted
/// by whichever merge a caller happens to run first. Non-positive spans are
/// illegal too: they only arise from offsets that overflowed the frame
/// conversion, and a merge must refuse rather than index by them.
bool mergedSpanIsLegal(int frames) =>
    frames > 0 && frames <= maxMergedLaneFrames;

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
/// null when nothing survives or when two or more parts span past
/// [maxMergedLaneFrames] (or past the cell budget), and a lone survivor
/// starting at the lane origin unchanged. Pure.
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
      mergeFrameOf(offsetMs, first.sampleRate, first.hop);
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
  // short clips hours apart would allocate gigabytes of gap columns. Two
  // fail-soft bounds (no merged view rather than an OOM): the shared span
  // budget, which the voice-metrics merge of the same lane applies
  // identically so neither merge can admit a lane the other refuses, and a
  // cell budget for the WIDTH — a wide-bin display merge reaches 256 MB well
  // inside a legal span. Span first, so a saturated frame count cannot wrap
  // `total * bins` negative and sail past the cell budget.
  const maxMergedCells = 64 * 1000 * 1000; // Float32 cells ≈ 256 MB
  if (!mergedSpanIsLegal(total) || total * bins > maxMergedCells) return null;
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
