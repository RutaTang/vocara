import 'dart:async';
import 'dart:isolate';
import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';

import '../../../domain/models/spectrogram.dart';
import '../../../domain/models/voice_metrics.dart';
import '../themes/studio.dart';

/// Colors for the first three formants (F1 blue, F2 green, F3 amber) — matches
/// the Estill Voiceprint convention and reads on the dark spectrogram.
const List<Color> formantColors = [
  Color(0xFF5B8DEF),
  Color(0xFF52B788),
  Color(0xFFE0A050),
];

// For the 2048-pt Hanning STFT used app-wide, a full-scale tone reads ~54 dB
// (20·log10(fftSize/4)). The color ceiling is clamped to at least this so an
// idle/near-silent input keeps its noise floor dark instead of being stretched
// to full brightness; once there's real voice the peak rises above it and the
// scale tracks the signal normally. Fixes the "all bright until you speak" live
// startup artifact.
const double _minCeilDb = 48;

// Heat colormap stops (dark -> indigo -> purple -> coral -> amber -> white).
const _stops = <List<int>>[
  [14, 14, 18],
  [28, 26, 60],
  [92, 40, 112],
  [224, 122, 106],
  [251, 191, 36],
  [255, 255, 255],
];

void _heat(double t, Uint8List px, int idx) {
  final n = _stops.length - 1;
  // A NON-FINITE t fails BOTH comparisons of the clamp and would reach
  // `floor()` as NaN, which throws UnsupportedError and takes the whole dock
  // down. Map it to the bottom of the scale — the "nothing measurable here"
  // colour, which is what an unusable bin should look like.
  final clamped = t.isFinite ? (t < 0 ? 0.0 : (t > 1 ? 1.0 : t)) : 0.0;
  final scaled = clamped * n;
  var i = scaled.floor();
  if (i >= n) i = n - 1;
  final f = scaled - i;
  final a = _stops[i];
  final b = _stops[i + 1];
  px[idx] = (a[0] + (b[0] - a[0]) * f).round();
  px[idx + 1] = (a[1] + (b[1] - a[1]) * f).round();
  px[idx + 2] = (a[2] + (b[2] - a[2]) * f).round();
  px[idx + 3] = 255;
}

/// The widest take spectrogram image, in columns. One column per analysis
/// frame is ~94 a second, so a take past ~3 minutes is wider than a GPU
/// texture holds (16384 on the Macs this runs on), and the engine then scales
/// the decoded image to fit — on the default (Skia) backend uniformly, which
/// cut a 10-minute take's 215 frequency rows to 62 and dropped frames to a
/// plain linear resample. Longer takes are max-pooled to this many columns
/// instead: every frequency row stays, and the loudest frame of each group
/// survives.
const int kMaxSpectrogramImageColumns = 16384;

/// The RGBA pixels of [s] as an image (low frequency at the bottom),
/// auto-scaled to a [dynamicRangeDb] window below the peak: one column per
/// frame, or — past [maxColumns] frames — [maxColumns] columns, column `c`
/// holding the per-bin maximum of frames `[c·frames/w, (c+1)·frames/w)`, so
/// each still covers an equal share of the take. Pure, so it runs on a worker
/// isolate; [s] must be non-empty.
@visibleForTesting
({Uint8List pixels, int width, int height}) spectrogramPixels(
  Spectrogram s, {
  double dynamicRangeDb = 72,
  int maxColumns = kMaxSpectrogramImageColumns,
}) {
  final frames = s.frames;
  final h = s.bins;
  final w = frames <= maxColumns ? frames : maxColumns;
  final pixels = Uint8List(w * h * 4);
  // A non-finite peak would make ceil, floor and range all NaN, so every bin
  // would come out non-finite rather than just the offending ones.
  final peak = s.maxDb.isFinite ? s.maxDb : _minCeilDb;
  final ceil = peak < _minCeilDb ? _minCeilDb : peak;
  final floor = ceil - dynamicRangeDb;
  final range = (ceil - floor) <= 0 ? 1.0 : (ceil - floor);

  if (w == frames) {
    for (var f = 0; f < w; f++) {
      final base = f * h;
      for (var b = 0; b < h; b++) {
        final t = (s.db[base + b] - floor) / range;
        final idx = ((h - 1 - b) * w + f) * 4; // flip: low freq at bottom
        _heat(t, pixels, idx);
      }
    }
    return (pixels: pixels, width: w, height: h);
  }

  // Max-pool. A NaN bin loses every comparison, so a group of only NaN stays
  // at -infinity — non-finite, which _heat paints like the NaN itself.
  final maxDb = Float64List(h);
  for (var c = 0; c < w; c++) {
    maxDb.fillRange(0, h, double.negativeInfinity);
    final last = (c + 1) * frames ~/ w;
    for (var f = c * frames ~/ w; f < last; f++) {
      final base = f * h;
      for (var b = 0; b < h; b++) {
        final v = s.db[base + b];
        if (v > maxDb[b]) maxDb[b] = v;
      }
    }
    for (var b = 0; b < h; b++) {
      final t = (maxDb[b] - floor) / range;
      _heat(t, pixels, ((h - 1 - b) * w + c) * 4);
    }
  }
  return (pixels: pixels, width: w, height: h);
}

/// Renders a [Spectrogram] to an RGBA image (low frequency at the bottom),
/// auto-scaled to a [dynamicRangeDb] window below the peak, at most
/// [kMaxSpectrogramImageColumns] wide (see [spectrogramPixels]).
Future<ui.Image> renderSpectrogramImage(
  Spectrogram s, {
  double dynamicRangeDb = 72,
}) async {
  // An empty spectrogram (frames/bins == 0, e.g. a not-yet-analysed or silent
  // take) would hand decodeImageFromPixels a zero-size buffer, which never
  // completes / throws. Emit a 1x1 transparent placeholder instead.
  if (s.frames == 0 || s.bins == 0) {
    final blank = Completer<ui.Image>();
    ui.decodeImageFromPixels(
      Uint8List(4),
      1,
      1,
      ui.PixelFormat.rgba8888,
      blank.complete,
    );
    return blank.future;
  }
  // A colour lookup per bin per frame — ~12M for a 10-minute take, hundreds
  // of ms that froze the UI on every take selection — so off the UI isolate.
  final px = await Isolate.run(
    () => spectrogramPixels(s, dynamicRangeDb: dynamicRangeDb),
  );
  final completer = Completer<ui.Image>();
  ui.decodeImageFromPixels(
    px.pixels,
    px.width,
    px.height,
    ui.PixelFormat.rgba8888,
    completer.complete,
  );
  return completer.future;
}

/// Rendered take spectrograms by [Spectrogram] identity (analyses are never
/// mutated; a new one is a new object). A take's image is rendered once and
/// shared: the dock tab and the take's inline lane show the same one, and
/// switching tabs or folding a lane open no longer renders it again.
///
/// Keyed WEAKLY: the analysis cache owns a spectrogram's lifetime under its
/// byte budget, and a strong key here kept every recently shown one (tens to
/// hundreds of MB each) resident after the cache let it go — and after the
/// practice was closed — until four newer takes had been rendered.
final Expando<Future<ui.Image>> _spectrogramImages = Expando();

/// The most recently requested renders, oldest first: the cache's own image
/// handles live here, at most [_spectrogramImageCacheSize] textures of up to
/// [kMaxSpectrogramImageColumns] × bins pixels.
final List<(WeakReference<Spectrogram>, Future<ui.Image>)> _recentImages = [];
const int _spectrogramImageCacheSize = 4;

/// A handle on [s]'s rendered image (see [renderSpectrogramImage]), shared
/// with every other caller for the same spectrogram. The caller owns the
/// returned handle and disposes it; the cache keeps its own until evicted.
Future<ui.Image> spectrogramImageFor(Spectrogram s) async {
  var image = _spectrogramImages[s];
  if (image == null) {
    final rendering = image = renderSpectrogramImage(s);
    _spectrogramImages[s] = rendering;
    // A failed render isn't kept: the next caller tries again.
    final key = WeakReference(s);
    unawaited(
      rendering.then<void>(
        (_) {},
        onError: (Object _) {
          final target = key.target;
          if (target != null &&
              identical(_spectrogramImages[target], rendering)) {
            _spectrogramImages[target] = null;
          }
          _recentImages.removeWhere((e) => identical(e.$2, rendering));
        },
      ),
    );
  }
  final shared = image;
  _recentImages
    ..removeWhere((e) => identical(e.$2, shared))
    ..add((WeakReference(s), shared)); // most recently used
  while (_recentImages.length > _spectrogramImageCacheSize) {
    final (key, evicted) = _recentImages.removeAt(0);
    final target = key.target;
    if (target != null && identical(_spectrogramImages[target], evicted)) {
      _spectrogramImages[target] = null;
    }
    // Handles already cloned off it stay valid: an image's memory goes with
    // its last handle.
    unawaited(evicted.then((i) => i.dispose(), onError: (Object _) {}));
  }
  return (await shared).clone();
}

/// Brightness multiplier applied to unvoiced live columns — a voiceprint should
/// read dark when there is no voice, so room noise/breath that scrolls by is
/// pushed toward black instead of glowing at the auto-scaled floor.
const double _idleColumnDim = 0.32;

/// The colour ceiling for a live window: its loudest bin, but at least
/// [_minCeilDb] (see there).
double _liveCeil(Iterable<double> columnPeaks) {
  var ceil = -1e9;
  for (final v in columnPeaks) {
    if (v > ceil) ceil = v;
  }
  return ceil < _minCeilDb ? _minCeilDb : ceil;
}

/// A live column's loudest bin (-1e9 when it has none).
double _columnPeak(Float32List col) {
  var peak = -1e9;
  for (final v in col) {
    if (v > peak) peak = v;
  }
  return peak;
}

/// Paints live column [col] into image column [x] of [pixels] ([w] wide, one
/// row per bin, low frequency at the bottom), dimmed when unvoiced.
void _paintLiveColumn(
  Uint8List pixels,
  int w,
  int bins,
  int x,
  Float32List col,
  bool unvoiced,
  double floor,
  double range,
) {
  final dim = unvoiced ? _idleColumnDim : 1.0;
  for (var b = 0; b < bins; b++) {
    final t = (col[b] - floor) / range * dim;
    _heat(t, pixels, ((bins - 1 - b) * w + x) * 4);
  }
}

/// The RGBA pixels [renderColumnsImage] decodes: [columns] (each length
/// [bins]) oldest on the left, auto-scaled below the peak, unvoiced columns
/// (0 in [active]) dimmed. [columns] must be non-empty.
@visibleForTesting
Uint8List liveColumnsPixels(
  List<Float32List> columns,
  int bins, {
  double dynamicRangeDb = 72,
  List<int>? active,
}) {
  final w = columns.length;
  final pixels = Uint8List(w * bins * 4);
  final ceil = _liveCeil(columns.map(_columnPeak));
  final floor = ceil - dynamicRangeDb;
  final range = (ceil - floor) <= 0 ? 1.0 : (ceil - floor);
  for (var f = 0; f < w; f++) {
    final unvoiced = active != null && f < active.length && active[f] == 0;
    _paintLiveColumn(pixels, w, bins, f, columns[f], unvoiced, floor, range);
  }
  return pixels;
}

/// Renders a live rolling window of dB [columns] (each length [bins]) to an
/// RGBA image, oldest column on the left. Auto-scaled below the peak. When
/// [active] is supplied (1 = voiced, aligned with [columns]), unvoiced columns
/// are dimmed so silence/noise reads dark.
Future<ui.Image> renderColumnsImage(
  List<Float32List> columns,
  int bins, {
  double dynamicRangeDb = 72,
  List<int>? active,
}) {
  // Same zero-size guard as renderSpectrogramImage, for the same reason: a
  // zero-size buffer makes decodeImageFromPixels never complete, and the only
  // caller AWAITS this — so the live view would freeze on its last image with
  // nothing to say why. Not reachable today (the caller returns early on no
  // columns, and bins is a positive constant of the analyser's FFT size); it
  // is here so the two renderers cannot disagree about it.
  if (columns.isEmpty || bins <= 0) {
    return decodeRgbaImage(Uint8List(4), 1, 1);
  }
  return decodeRgbaImage(
    liveColumnsPixels(
      columns,
      bins,
      dynamicRangeDb: dynamicRangeDb,
      active: active,
    ),
    columns.length,
    bins,
  );
}

/// Decodes [width] × [height] RGBA [pixels] into an image. A zero-size image
/// decodes as a 1x1 transparent placeholder: decodeImageFromPixels never
/// completes on a zero-size buffer, and the callers await it.
Future<ui.Image> decodeRgbaImage(Uint8List pixels, int width, int height) {
  if (width <= 0 || height <= 0) {
    pixels = Uint8List(4);
    width = 1;
    height = 1;
  }
  final completer = Completer<ui.Image>();
  ui.decodeImageFromPixels(
    pixels,
    width,
    height,
    ui.PixelFormat.rgba8888,
    completer.complete,
  );
  return completer.future;
}

/// [liveColumnsPixels] for the live monitor's rolling window, kept up to date
/// incrementally. The window only ever gains columns on the right and loses
/// them on the left, and while its colour ceiling (its loudest bin) holds, a
/// column already on screen keeps its colours — so an update shifts the
/// previous pixels left and colours only the new columns, instead of the
/// whole window (720 × 215 bins) on every frame. When the ceiling moves, or
/// the window isn't a continuation of the last one, it repaints in full.
/// Either way the pixels are exactly [liveColumnsPixels]'.
class LiveColumnsRaster {
  LiveColumnsRaster({this.dynamicRangeDb = 72});

  final double dynamicRangeDb;

  // The last update's window, as given (the monitor's columns are stable
  // objects: a column is appended once and evicted once).
  List<Float32List> _columns = const [];
  List<int>? _active;
  int _bins = 0;
  double _ceil = double.nan;
  Uint8List _pixels = Uint8List(0);

  // Each column's loudest bin, computed once per column.
  final Expando<double> _peaks = Expando('liveColumnPeak');

  /// Pixels for [columns] (non-empty) as [liveColumnsPixels] would paint
  /// them, in a buffer the caller owns.
  Uint8List update(List<Float32List> columns, int bins, {List<int>? active}) {
    final w = columns.length;
    final ceil = _liveCeil(columns.map((c) => _peaks[c] ??= _columnPeak(c)));
    final floor = ceil - dynamicRangeDb;
    final range = (ceil - floor) <= 0 ? 1.0 : (ceil - floor);
    bool unvoiced(int f) =>
        active != null && f < active.length && active[f] == 0;

    final kept = (bins == _bins && ceil == _ceil)
        ? _continuation(columns, active)
        : null;
    final Uint8List pixels;
    var from = 0;
    if (kept == null) {
      pixels = Uint8List(w * bins * 4);
    } else {
      // Shift the columns still on screen to the left edge, row by row.
      final (evicted, overlap) = kept;
      final oldW = _columns.length;
      pixels = w == oldW ? _pixels : Uint8List(w * bins * 4);
      for (var row = 0; row < bins; row++) {
        pixels.setRange(
          row * w * 4,
          (row * w + overlap) * 4,
          _pixels,
          (row * oldW + evicted) * 4,
        );
      }
      from = overlap;
    }
    for (var f = from; f < w; f++) {
      _paintLiveColumn(
        pixels,
        w,
        bins,
        f,
        columns[f],
        unvoiced(f),
        floor,
        range,
      );
    }

    _columns = List.of(columns);
    _active = active == null ? null : List.of(active);
    _bins = bins;
    _ceil = ceil;
    _pixels = pixels;
    return Uint8List.fromList(pixels);
  }

  /// When [columns] continues the last window — its first columns are the
  /// last window's, from some point on, in order, with the same voicing —
  /// how many columns were evicted from the left and how many carry over.
  (int, int)? _continuation(List<Float32List> columns, List<int>? active) {
    final old = _columns;
    if (old.isEmpty || columns.isEmpty) return null;
    final evicted = old.indexWhere((c) => identical(c, columns.first));
    if (evicted < 0) return null;
    final overlap = old.length - evicted;
    if (overlap > columns.length) return null;
    bool unvoicedNow(int f) =>
        active != null && f < active.length && active[f] == 0;
    final oldActive = _active;
    bool unvoicedThen(int f) =>
        oldActive != null && f < oldActive.length && oldActive[f] == 0;
    for (var i = 0; i < overlap; i++) {
      if (!identical(old[evicted + i], columns[i])) return null;
      if (unvoicedThen(evicted + i) != unvoicedNow(i)) return null;
    }
    return (evicted, overlap);
  }
}

/// Renders a spectrogram image asynchronously and paints it scaled to fill the
/// available area. A track shorter than the timeline only fills [widthFraction].
class SpectrogramImage extends StatefulWidget {
  const SpectrogramImage({
    required this.spectrogram,
    this.widthFraction = 1.0,
    super.key,
  });

  final Spectrogram spectrogram;
  final double widthFraction;

  @override
  State<SpectrogramImage> createState() => _SpectrogramImageState();
}

class _SpectrogramImageState extends State<SpectrogramImage> {
  ui.Image? _image;
  int _generation = 0;

  @override
  void initState() {
    super.initState();
    _render();
  }

  @override
  void didUpdateWidget(SpectrogramImage old) {
    super.didUpdateWidget(old);
    if (!identical(old.spectrogram, widget.spectrogram)) {
      _render();
    }
  }

  Future<void> _render() async {
    final gen = ++_generation;
    final ui.Image image;
    try {
      image = await spectrogramImageFor(widget.spectrogram);
    } catch (_) {
      return; // nothing to show; a later mount renders again
    }
    // Drop a stale render: the widget was disposed, OR a newer _render started
    // (rapid take switches, whose decodes don't necessarily complete in order),
    // so this image is no longer the current one. Without the generation check an
    // earlier decode completing last would persistently show the wrong take.
    if (!mounted || gen != _generation) {
      image.dispose();
      return;
    }
    setState(() {
      _image?.dispose();
      _image = image;
    });
  }

  @override
  void dispose() {
    _image?.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final image = _image;
    if (image == null) return const SizedBox.expand();
    return CustomPaint(
      size: Size.infinite,
      painter: _SpectrogramImagePainter(image, widget.widthFraction),
    );
  }
}

class _SpectrogramImagePainter extends CustomPainter {
  _SpectrogramImagePainter(this.image, this.widthFraction);

  final ui.Image image;
  final double widthFraction;

  @override
  void paint(Canvas canvas, Size size) {
    final src = Rect.fromLTWH(
      0,
      0,
      image.width.toDouble(),
      image.height.toDouble(),
    );
    final dst = Rect.fromLTWH(0, 0, size.width * widthFraction, size.height);
    canvas.drawImageRect(
      image,
      src,
      dst,
      Paint()..filterQuality = FilterQuality.medium,
    );
  }

  @override
  bool shouldRepaint(_SpectrogramImagePainter old) =>
      old.image != image || old.widthFraction != widthFraction;
}

/// Horizontal frequency grid (1–5 kHz) + the Singer's Formant band overlay,
/// drawn over the spectrogram. [maxHz] sets the vertical scale.
class FrequencyGridPainter extends CustomPainter {
  FrequencyGridPainter({
    required this.maxHz,
    this.singersFormantLow = 2600,
    this.singersFormantHigh = 3800,
  });

  final double maxHz;
  final double singersFormantLow;
  final double singersFormantHigh;

  double _y(double hz, double height) => height * (1 - hz / maxHz);

  @override
  void paint(Canvas canvas, Size size) {
    // Singer's Formant band.
    final top = _y(singersFormantHigh, size.height);
    final bottom = _y(singersFormantLow, size.height);
    canvas.drawRect(
      Rect.fromLTRB(0, top, size.width, bottom),
      Paint()..color = Studio.accent.withValues(alpha: 0.10),
    );

    final line = Paint()
      ..color = Studio.textPrimary.withValues(alpha: 0.12)
      ..strokeWidth = 1;
    for (var hz = 1000.0; hz < maxHz; hz += 1000) {
      final y = _y(hz, size.height);
      canvas.drawLine(Offset(0, y), Offset(size.width, y), line);
    }
  }

  @override
  bool shouldRepaint(FrequencyGridPainter old) =>
      old.maxHz != maxHz ||
      old.singersFormantLow != singersFormantLow ||
      old.singersFormantHigh != singersFormantHigh;
}

/// Draws the instantaneous power spectrum (frequency X, dB Y) for one column,
/// with a smoothed formant envelope and the Singer's Formant region shaded.
class PowerSpectrumPainter extends CustomPainter {
  PowerSpectrumPainter({
    required this.column,
    required this.binHz,
    required this.maxHz,
    this.formants = const [],
    this.dynamicRangeDb = 78,
    this.minCeilDb,
  });

  final Float32List column;
  final double binHz;
  final double maxHz;

  /// Estimated formant frequencies (Hz) at this moment, drawn as labeled
  /// vertical markers — the real F1/F2/F3, vs the merely-smoothed envelope.
  final List<double> formants;
  final double dynamicRangeDb;

  /// Optional floor for the auto-scaled ceiling. Live mic input sets this so a
  /// silent frame stays a low flat line instead of stretching its noise floor
  /// to fill the panel; the take view leaves it null (always auto-scale).
  final double? minCeilDb;

  @override
  void paint(Canvas canvas, Size size) {
    final n = column.length;
    if (n == 0) return;

    // The power spectrum is a single-moment detail view — it auto-scales to its
    // own column's peak so the envelope is always visible. Live input clamps the
    // ceiling to [minCeilDb] so near-silence reads as a low flat line.
    var ceil = -1e9;
    for (final v in column) {
      if (v > ceil) ceil = v;
    }
    if (minCeilDb != null && ceil < minCeilDb!) ceil = minCeilDb!;
    final floor = ceil - dynamicRangeDb;
    final range = ceil - floor <= 0 ? 1.0 : ceil - floor;

    double x(int bin) => (bin * binHz / maxHz).clamp(0.0, 1.0) * size.width;
    double y(double db) {
      final t = ((db - floor) / range).clamp(0.0, 1.0);
      return size.height * (1 - t);
    }

    // Singer's Formant region shading.
    canvas.drawRect(
      Rect.fromLTRB(
        x((2600 / binHz).round()),
        0,
        x((3800 / binHz).round()),
        size.height,
      ),
      Paint()..color = Studio.accent.withValues(alpha: 0.08),
    );

    // Raw spectrum as a filled area.
    final area = Path()..moveTo(0, size.height);
    for (var b = 0; b < n; b++) {
      area.lineTo(x(b), y(column[b]));
    }
    area
      ..lineTo(x(n - 1), size.height)
      ..close();
    canvas.drawPath(
      area,
      Paint()..color = Studio.waveform.withValues(alpha: 0.35),
    );

    // Smoothed formant envelope.
    const half = 6;
    final env = Path();
    var started = false;
    for (var b = 0; b < n; b++) {
      var sum = 0.0;
      var count = 0;
      for (var k = b - half; k <= b + half; k++) {
        if (k >= 0 && k < n) {
          sum += column[k];
          count++;
        }
      }
      final yy = y(sum / count);
      if (!started) {
        env.moveTo(x(b), yy);
        started = true;
      } else {
        env.lineTo(x(b), yy);
      }
    }
    canvas.drawPath(
      env,
      Paint()
        ..color = Studio.accent.withValues(alpha: 0.5)
        ..style = PaintingStyle.stroke
        ..strokeWidth = 1.5,
    );

    // Real formant markers (F1/F2/F3) as labeled vertical lines.
    for (var i = 0; i < formants.length && i < 3; i++) {
      final hz = formants[i];
      if (hz.isNaN || hz <= 0) continue;
      final fx = (hz / maxHz).clamp(0.0, 1.0) * size.width;
      final color = formantColors[i];
      canvas.drawLine(
        Offset(fx, 0),
        Offset(fx, size.height),
        Paint()
          ..color = color.withValues(alpha: 0.85)
          ..strokeWidth = 1.5,
      );
      final tp = TextPainter(
        text: TextSpan(
          text: 'F${i + 1}',
          style: TextStyle(
            color: color,
            fontSize: 9,
            fontWeight: FontWeight.w700,
          ),
        ),
        textDirection: TextDirection.ltr,
      )..layout();
      tp.paint(canvas, Offset(fx + 2, 2));
    }
  }

  @override
  bool shouldRepaint(PowerSpectrumPainter old) =>
      old.column != column ||
      old.binHz != binHz ||
      old.maxHz != maxHz ||
      old.minCeilDb != minCeilDb ||
      !listEquals(old.formants, formants);
}

/// The F0 (pitch) overlay color — bright white, distinct from the formants.
const Color f0Color = Color(0xFFF2F2F8);

/// Overlays the F0 pitch track and the F1/F2/F3 formant tracks (F17) on the
/// spectrogram: one dot per frame, time across X and frequency up Y. F0 is drawn
/// in white; formants in [formantColors].
class FormantTrackPainter extends CustomPainter {
  FormantTrackPainter({required this.metrics, required this.maxHz});

  final VoiceMetrics metrics;
  final double maxHz;

  @override
  void paint(Canvas canvas, Size size) {
    final frames = metrics.frames;
    if (frames == 0) return;
    // One round point per voiced frame, drawn as a single point batch per
    // track: a round-capped point [radius]·2 wide is the same filled dot as
    // drawCircle, but a 10-minute take is ~56k frames per track, and as
    // ~225k separate drawCircle calls it cost tens of ms to record.
    final points = Float32List(frames * 2);
    void track(Float32List f, Color color, double radius) {
      var n = 0;
      for (var fr = 0; fr < frames; fr++) {
        final hz = f[fr];
        if (hz.isNaN || hz <= 0 || hz > maxHz) continue;
        // Cell-centered, matching the spectrogram image's per-column stretch
        // (column c centers at (c+0.5)/frames·width) so dots sit on the ridges.
        points[n++] = (fr + 0.5) / frames * size.width;
        points[n++] = (1 - hz / maxHz) * size.height;
      }
      if (n == 0) return;
      canvas.drawRawPoints(
        ui.PointMode.points,
        Float32List.sublistView(points, 0, n),
        Paint()
          ..color = color.withValues(alpha: 0.9)
          ..strokeWidth = radius * 2
          ..strokeCap = StrokeCap.round,
      );
    }

    track(metrics.f1, formantColors[0], 1.1);
    track(metrics.f2, formantColors[1], 1.1);
    track(metrics.f3, formantColors[2], 1.1);
    track(metrics.f0, f0Color, 1.4); // F0 on top, slightly larger
  }

  @override
  bool shouldRepaint(FormantTrackPainter old) =>
      old.metrics != metrics || old.maxHz != maxHz;
}

/// Live equivalent of [FormantTrackPainter]: draws rolling F0/F1/F2/F3 histories
/// (one value per spectrogram column) over the scrolling live spectrogram. The
/// lists are mutated in place by the monitor, so it always repaints.
class LiveFormantPainter extends CustomPainter {
  LiveFormantPainter({
    required this.f0,
    required this.f1,
    required this.f2,
    required this.f3,
    required this.maxHz,
  });

  final List<double> f0, f1, f2, f3;
  final double maxHz;

  @override
  void paint(Canvas canvas, Size size) {
    void track(List<double> f, Color color, double radius) {
      final n = f.length;
      if (n == 0) return;
      final paint = Paint()..color = color.withValues(alpha: 0.9);
      for (var c = 0; c < n; c++) {
        final hz = f[c];
        if (hz.isNaN || hz <= 0 || hz > maxHz) continue;
        final x = (c + 0.5) / n * size.width;
        final y = (1 - hz / maxHz) * size.height;
        canvas.drawCircle(Offset(x, y), radius, paint);
      }
    }

    track(f1, formantColors[0], 1.1);
    track(f2, formantColors[1], 1.1);
    track(f3, formantColors[2], 1.1);
    track(f0, f0Color, 1.4);
  }

  @override
  bool shouldRepaint(LiveFormantPainter old) => true;
}

/// An F1×F2 vowel chart (F17): reference vowels as faint labels plus a coral dot
/// at the current frame's formants. High F2 sits left and high F1 sits low, the
/// usual IPA-style layout.
class VowelSpacePainter extends CustomPainter {
  VowelSpacePainter({required this.f1, required this.f2});

  final double f1;
  final double f2;

  static const double _f1Lo = 200, _f1Hi = 1000;
  static const double _f2Lo = 600, _f2Hi = 2700;

  // Reference vowels (label, F1, F2) — neutral adult values.
  static const List<(String, double, double)> _refs = [
    ('i', 300, 2300),
    ('e', 500, 1900),
    ('a', 750, 1150),
    ('o', 450, 850),
    ('u', 350, 750),
  ];

  Offset _pos(double f1v, double f2v, Size size) {
    final x =
        (1 - (f2v - _f2Lo) / (_f2Hi - _f2Lo)).clamp(0.0, 1.0) * size.width;
    final y = ((f1v - _f1Lo) / (_f1Hi - _f1Lo)).clamp(0.0, 1.0) * size.height;
    return Offset(x, y);
  }

  @override
  void paint(Canvas canvas, Size size) {
    canvas.drawRect(
      Offset.zero & size,
      Paint()
        ..color = Studio.border
        ..style = PaintingStyle.stroke
        ..strokeWidth = 1,
    );

    for (final (label, rf1, rf2) in _refs) {
      final p = _pos(rf1, rf2, size);
      canvas.drawCircle(
        p,
        2,
        Paint()..color = Studio.textFaint.withValues(alpha: 0.5),
      );
      final tp = TextPainter(
        text: TextSpan(
          text: label,
          style: const TextStyle(color: Studio.textFaint, fontSize: 9),
        ),
        textDirection: TextDirection.ltr,
      )..layout();
      tp.paint(canvas, p + const Offset(3, -4));
    }

    if (!f1.isNaN && !f2.isNaN && f1 > 0 && f2 > 0) {
      final p = _pos(f1, f2, size);
      canvas.drawCircle(p, 4, Paint()..color = Studio.accent);
      canvas.drawCircle(
        p,
        4,
        Paint()
          ..color = Studio.background
          ..style = PaintingStyle.stroke
          ..strokeWidth = 1.2,
      );
    }
  }

  @override
  bool shouldRepaint(VowelSpacePainter old) => old.f1 != f1 || old.f2 != f2;
}
