import 'dart:typed_data';

/// The STFT framing every per-frame analysis is laid out on, and the ONE
/// frame↔time mapping all of them share. Frame f analyses the [fftSize]-sample
/// window that STARTS at sample f·[hop] — the spectrogram column and every
/// voice metric of that frame (YIN and LPC run on windows centred on the same
/// point) describe the audio around the window's CENTRE, f·hop + fftSize/2.
///
/// So a frame is placed at its centre, never at its start: a start-anchored
/// frame reads every onset half a window (~21 ms at 2048/48 kHz) EARLY, so the
/// pitch, loudness and spectrogram views led the waveform and the playhead
/// read a frame two hops ahead of the audio under it. Everything that turns a
/// frame into a time, or a time into a frame — painters, readouts, seeks —
/// goes through here so drawing and reading cannot disagree.
mixin AnalysisFraming {
  int get frames;
  int get hop;
  int get fftSize;
  int get sampleRate;

  /// Seconds from the analysed audio's start to the centre of [frame]'s
  /// analysis window — the moment that frame describes.
  double timeOfFrame(int frame) => (frame * hop + fftSize / 2) / sampleRate;

  /// The fractional, unclamped frame index whose window centre sits at
  /// [seconds] — the exact inverse of [timeOfFrame]. For callers that need a
  /// floor or ceiling (a visible frame window) rather than the nearest frame.
  double framePosition(double seconds) =>
      (seconds * sampleRate - fftSize / 2) / hop;

  /// The frame whose window centre is nearest [seconds], clamped to the frame
  /// range (0 when there are no frames).
  int frameAtTime(double seconds) {
    if (frames == 0) return 0;
    var f = framePosition(seconds).round();
    if (f < 0) f = 0;
    if (f >= frames) f = frames - 1;
    return f;
  }

  /// Where [frame]'s cell begins and ends (seconds) when frames are drawn as
  /// contiguous hop-wide columns: centred on [timeOfFrame], so a column's
  /// middle is the moment it describes, and [frameAtTime] of any time inside
  /// the cell answers [frame]. The spectrogram image spans
  /// `cellStart(0)..cellEnd(frames - 1)` — NOT the take's whole length, which
  /// would stretch the columns and drift them along the time axis.
  double cellStart(int frame) => timeOfFrame(frame) - hop / 2 / sampleRate;
  double cellEnd(int frame) => timeOfFrame(frame) + hop / 2 / sampleRate;
}

/// A computed spectrogram: a [frames] × [bins] grid of magnitudes in dB,
/// covering 0..[maxHz]. Row-major: `db[frame * bins + bin]`. Frequency is
/// linear in bin index (bin b ≈ b * sampleRate / fftSize Hz). Frame times
/// follow [AnalysisFraming].
class Spectrogram with AnalysisFraming {
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

  @override
  final int frames;
  final int bins;
  final double maxHz;
  @override
  final int sampleRate;
  @override
  final int fftSize;
  @override
  final int hop;

  /// Magnitudes in dB, row-major (`frame * bins + bin`).
  final Float32List db;

  final double minDb;
  final double maxDb;

  double freqOfBin(int bin) => bin * sampleRate / fftSize;
  double get binHz => sampleRate / fftSize;

  /// dB value at a frame/bin (clamped to range).
  double valueAt(int frame, int bin) => db[frame * bins + bin];

  /// The column (dB per bin) whose window is centred nearest [seconds]; empty
  /// if no frames.
  Float32List columnAtTime(double seconds) {
    if (frames == 0) return Float32List(0);
    final frame = frameAtTime(seconds);
    return Float32List.sublistView(db, frame * bins, frame * bins + bins);
  }
}

/// The frame a part starting [offsetMs] after the lane's span start occupies
/// on the merged grid, for a [sampleRate]/[hop] framing. An OFFSET, so no
/// half-window term: every frame of the part keeps its own window-centre
/// placement, shifted whole by the part's start. The spectrogram and
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
/// frames are placed at that offset, so frame f of the result's window always
/// starts f·hop/sampleRate seconds after the lane start — its [timeOfFrame]
/// is lane-relative exactly as each part's is clip-relative. Gaps between
/// clips become floor (merged minDb) columns, which the heat renderer draws
/// as silence; where clips overlap, the later-starting clip wins.
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
