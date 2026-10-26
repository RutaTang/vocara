import 'dart:async';
import 'dart:math' as math;
import 'dart:typed_data';

import 'package:flutter/foundation.dart' show listEquals;
import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/scheduler.dart';

import '../../../domain/models/audio_fx.dart';
import '../../../domain/models/audio_track.dart';
import '../../../domain/models/fx_bus.dart';
import '../../../domain/models/fx_presets.dart';
import '../../core/themes/studio.dart';
import '../../core/widgets/cap_label.dart';
import '../../core/widgets/studio_controls.dart' show FixedWidthReadout;
import '../view_model/song_practice_detail_view_model.dart';
import 'track_context_menu.dart';

const double _eqMinHz = 20;
const double _eqMaxHz = 18000;
const double _eqMaxDb = 15;
const double _graphSr = 48000;

// --- RBJ EQ magnitude response (mirrors the Rust engine so the curve matches the sound) ---

({double b0, double b1, double b2, double a1, double a2}) _coeffs(EqBand band) {
  final f0 = band.freq.clamp(10.0, _graphSr * 0.45);
  final q = math.max(band.q, 0.05);
  final a = math.pow(10, band.gainDb / 40).toDouble();
  final w0 = 2 * math.pi * f0 / _graphSr;
  final cosw = math.cos(w0);
  final sinw = math.sin(w0);
  final alpha = sinw / (2 * q);
  double b0, b1, b2, a0, a1, a2;
  switch (band.kind) {
    case EqBandKind.peaking:
      b0 = 1 + alpha * a;
      b1 = -2 * cosw;
      b2 = 1 - alpha * a;
      a0 = 1 + alpha / a;
      a1 = -2 * cosw;
      a2 = 1 - alpha / a;
    case EqBandKind.lowShelf:
      final s = 2 * math.sqrt(a) * alpha;
      b0 = a * ((a + 1) - (a - 1) * cosw + s);
      b1 = 2 * a * ((a - 1) - (a + 1) * cosw);
      b2 = a * ((a + 1) - (a - 1) * cosw - s);
      a0 = (a + 1) + (a - 1) * cosw + s;
      a1 = -2 * ((a - 1) + (a + 1) * cosw);
      a2 = (a + 1) + (a - 1) * cosw - s;
    case EqBandKind.highShelf:
      final s = 2 * math.sqrt(a) * alpha;
      b0 = a * ((a + 1) + (a - 1) * cosw + s);
      b1 = -2 * a * ((a - 1) + (a + 1) * cosw);
      b2 = a * ((a + 1) + (a - 1) * cosw - s);
      a0 = (a + 1) - (a - 1) * cosw + s;
      a1 = 2 * ((a - 1) - (a + 1) * cosw);
      a2 = (a + 1) - (a - 1) * cosw - s;
  }
  return (b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0);
}

double _bandDb(EqBand band, double f) {
  if (!band.enabled || band.gainDb == 0) return 0;
  final c = _coeffs(band);
  final w = 2 * math.pi * f / _graphSr;
  final cosw = math.cos(w), cos2w = math.cos(2 * w);
  final sinw = math.sin(w), sin2w = math.sin(2 * w);
  final numR = c.b0 + c.b1 * cosw + c.b2 * cos2w;
  final numI = -(c.b1 * sinw + c.b2 * sin2w);
  final denR = 1 + c.a1 * cosw + c.a2 * cos2w;
  final denI = -(c.a1 * sinw + c.a2 * sin2w);
  final num = math.sqrt(numR * numR + numI * numI);
  final den = math.sqrt(denR * denR + denI * denI);
  if (den == 0) return 0;
  return 20 * (math.log(num / den) / math.ln10);
}

double _totalDb(List<EqBand> bands, bool on, double f) {
  if (!on) return 0;
  var sum = 0.0;
  for (final b in bands) {
    sum += _bandDb(b, f);
  }
  return sum;
}

double _freqToX(double f, double w) =>
    (math.log(f / _eqMinHz) / math.log(_eqMaxHz / _eqMinHz)) * w;
double _xToFreq(double x, double w) =>
    _eqMinHz *
    math.pow(_eqMaxHz / _eqMinHz, (x / w).clamp(0.0, 1.0)).toDouble();
double _dbToY(double db, double h) => h / 2 - (db / _eqMaxDb) * (h / 2);
double _yToDb(double y, double h) =>
    ((h / 2 - y) / (h / 2) * _eqMaxDb).clamp(-_eqMaxDb, _eqMaxDb);

// --- Spectrum analyzer (FabFilter-style input/output overlay behind the curve) ---

/// Render resolution of the spectrum across the width — log-spaced points, each
/// covering a constant fraction of an octave (so averaging bins per point gives
/// free octave smoothing).
const int _spectrumPoints = 168;

/// Centre frequency of spectrum render point [i].
double _spectrumPointFreq(int i) =>
    _eqMinHz *
    math.pow(_eqMaxHz / _eqMinHz, (i + 0.5) / _spectrumPoints).toDouble();

/// The FX detail for the selected vocal take: presets, an interactive EQ graph, compressor,
/// reverb send, and the shared reverb bus.
class FxPanel extends StatelessWidget {
  const FxPanel({required this.vm, this.track, this.bus, super.key})
    : assert(track != null || bus != null, 'FxPanel needs a track or a bus');

  final SongPracticeDetailViewModel vm;

  /// The FX target is exactly one of these: a vocal take's insert chain, or an
  /// FX bus's chain. Everything below is driven through the resolved getters.
  final AudioTrack? track;
  final FxBus? bus;

  bool get _isBus => bus != null;

  /// The `StableSubtree.sameAs` of an FX panel: everything it shows comes
  /// from its [track] or [bus] — compared by identity, as any edit replaces
  /// them — and what it reads from the view model it reads through its own
  /// listeners and polls, never from a build. So a fader, knob or clip drag
  /// elsewhere (every view-model notification rebuilds the inspector) leaves
  /// the whole insert chain alone.
  static bool same(FxPanel next, FxPanel shown) =>
      next.key == shown.key &&
      identical(next.vm, shown.vm) &&
      identical(next.track, shown.track) &&
      identical(next.bus, shown.bus);

  /// What wakes a take's idle EQ analyzer: the playhead, and the analysis
  /// caches — where a newly computed input spectrum arrives, which a rebuild
  /// of this panel (kept across unrelated ones, see [same]) is no longer sure
  /// to deliver. One per view model, so the analyzer keeps its listeners.
  static Listenable _analyzerWake(SongPracticeDetailViewModel vm) =>
      _analyzerWakes[vm] ??= Listenable.merge([vm.position, vm.analysis]);
  static final _analyzerWakes = Expando<Listenable>('FxPanel analyzer wake');

  TrackFx get _fx => _isBus ? bus!.fx : (track!.fx ?? TrackFx.flat());

  void _setFx(TrackFx fx, {bool commit = false}) {
    if (_isBus) {
      vm.mixer.setBusFxLive(bus!.id, fx);
    } else {
      vm.mixer.setTrackFxLive(track!.id, fx);
    }
    if (commit) vm.commitFx();
  }

  /// The compressor gain-reduction poll for the GR meter.
  double _gr() => _isBus
      ? vm.mixer.busGainReductionDb(bus!.id)
      : vm.mixer.gainReductionDb(track!.id);

  /// The pre-EQ input spectrum column for the analyzer — only a track has one
  /// (a bus is a sum of routed tracks, so it shows the response curve alone).
  ({Float32List col, double binHz})? _eqInput() =>
      _isBus ? null : vm.analysis.eqInputColumn(track!.id);

  /// One insert card in the chain: a header (drag handle + on/off) plus, when
  /// enabled, the plugin's editor. Keyed by kind so reordering animates.
  Widget _insertCard(int index, FxKind kind, TrackFx fx) {
    return Container(
      key: ValueKey(kind),
      // Opaque panel-colored background so cards fully occlude each other while
      // the reorder animation slides them past one another (no see-through flash).
      color: Studio.surface,
      padding: const EdgeInsets.only(bottom: 18),
      child: switch (kind) {
        FxKind.eq => Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            _insertHeader(
              index,
              'Equalizer',
              fx.eqOn,
              (v) => _setFx(fx.copyWith(eqOn: v), commit: true),
            ),
            if (fx.eqOn) ...[
              const SizedBox(height: 8),
              _EqGraph(
                inputColumn: _eqInput,
                wake: _isBus ? null : _analyzerWake(vm),
                bands: fx.eq,
                on: fx.eqOn,
                onChanged: (i, band) =>
                    _setFx(fx.withBand(i, band).copyWith(eqOn: true)),
                onCommit: vm.commitFx,
              ),
            ],
          ],
        ),
        FxKind.comp => Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            _insertHeader(
              index,
              'Compressor',
              fx.compOn,
              (v) => _setFx(fx.copyWith(compOn: v), commit: true),
            ),
            if (fx.compOn) ...[
              const SizedBox(height: 6),
              _slider(
                'Threshold',
                fx.comp.thresholdDb,
                -48,
                0,
                'dB',
                (v) =>
                    _setFx(fx.copyWith(comp: fx.comp.copyWith(thresholdDb: v))),
                vm.commitFx,
              ),
              _slider(
                'Ratio',
                fx.comp.ratio,
                1,
                12,
                ':1',
                (v) => _setFx(fx.copyWith(comp: fx.comp.copyWith(ratio: v))),
                vm.commitFx,
              ),
              _slider(
                'Makeup',
                fx.comp.makeupDb,
                0,
                18,
                'dB',
                (v) => _setFx(fx.copyWith(comp: fx.comp.copyWith(makeupDb: v))),
                vm.commitFx,
              ),
              const SizedBox(height: 2),
              _GrMeter(poll: _gr, wake: vm.position),
            ],
          ],
        ),
        FxKind.deEsser => _simpleInsert(
          index,
          'De-esser',
          fx.deEsserOn,
          (v) => _setFx(fx.copyWith(deEsserOn: v), commit: true),
          [
            _slider(
              'Freq',
              fx.deEsser.freq,
              2000,
              16000,
              'Hz',
              (v) => _setFx(fx.copyWith(deEsser: fx.deEsser.copyWith(freq: v))),
              vm.commitFx,
            ),
            _slider(
              'Threshold',
              fx.deEsser.thresholdDb,
              -60,
              0,
              'dB',
              (v) => _setFx(
                fx.copyWith(deEsser: fx.deEsser.copyWith(thresholdDb: v)),
              ),
              vm.commitFx,
            ),
            _slider(
              'Range',
              fx.deEsser.rangeDb,
              0,
              24,
              'dB',
              (v) =>
                  _setFx(fx.copyWith(deEsser: fx.deEsser.copyWith(rangeDb: v))),
              vm.commitFx,
            ),
          ],
        ),
        FxKind.gate => _simpleInsert(
          index,
          'Gate',
          fx.gateOn,
          (v) => _setFx(fx.copyWith(gateOn: v), commit: true),
          [
            _slider(
              'Threshold',
              fx.gate.thresholdDb,
              -80,
              0,
              'dB',
              (v) =>
                  _setFx(fx.copyWith(gate: fx.gate.copyWith(thresholdDb: v))),
              vm.commitFx,
            ),
            _slider(
              'Range',
              fx.gate.rangeDb,
              0,
              80,
              'dB',
              (v) => _setFx(fx.copyWith(gate: fx.gate.copyWith(rangeDb: v))),
              vm.commitFx,
            ),
            _slider(
              'Attack',
              fx.gate.attackMs,
              0.1,
              50,
              'ms',
              (v) => _setFx(fx.copyWith(gate: fx.gate.copyWith(attackMs: v))),
              vm.commitFx,
            ),
            _slider(
              'Release',
              fx.gate.releaseMs,
              10,
              500,
              'ms',
              (v) => _setFx(fx.copyWith(gate: fx.gate.copyWith(releaseMs: v))),
              vm.commitFx,
            ),
          ],
        ),
        FxKind.delay => _simpleInsert(
          index,
          'Delay',
          fx.delayOn,
          (v) => _setFx(fx.copyWith(delayOn: v), commit: true),
          [
            _slider(
              'Time',
              fx.delay.timeMs,
              1,
              1500,
              'ms',
              (v) => _setFx(fx.copyWith(delay: fx.delay.copyWith(timeMs: v))),
              vm.commitFx,
            ),
            _slider(
              'Feedback',
              fx.delay.feedback,
              0,
              0.95,
              '',
              (v) => _setFx(fx.copyWith(delay: fx.delay.copyWith(feedback: v))),
              vm.commitFx,
            ),
            _slider(
              'Mix',
              fx.delay.mix,
              0,
              1,
              '',
              (v) => _setFx(fx.copyWith(delay: fx.delay.copyWith(mix: v))),
              vm.commitFx,
            ),
          ],
        ),
        FxKind.saturation => _simpleInsert(
          index,
          'Saturation',
          fx.saturationOn,
          (v) => _setFx(fx.copyWith(saturationOn: v), commit: true),
          [
            _slider(
              'Drive',
              fx.saturation.driveDb,
              0,
              36,
              'dB',
              (v) => _setFx(
                fx.copyWith(saturation: fx.saturation.copyWith(driveDb: v)),
              ),
              vm.commitFx,
            ),
            _slider(
              'Mix',
              fx.saturation.mix,
              0,
              1,
              '',
              (v) => _setFx(
                fx.copyWith(saturation: fx.saturation.copyWith(mix: v)),
              ),
              vm.commitFx,
            ),
          ],
        ),
        FxKind.doubler => _simpleInsert(
          index,
          'Doubler',
          fx.doublerOn,
          (v) => _setFx(fx.copyWith(doublerOn: v), commit: true),
          [
            _slider(
              'Delay',
              fx.doubler.delayMs,
              1,
              50,
              'ms',
              (v) =>
                  _setFx(fx.copyWith(doubler: fx.doubler.copyWith(delayMs: v))),
              vm.commitFx,
            ),
            _slider(
              'Depth',
              fx.doubler.depthMs,
              0,
              20,
              'ms',
              (v) =>
                  _setFx(fx.copyWith(doubler: fx.doubler.copyWith(depthMs: v))),
              vm.commitFx,
            ),
            _slider(
              'Rate',
              fx.doubler.rateHz,
              0.05,
              8,
              'Hz',
              (v) =>
                  _setFx(fx.copyWith(doubler: fx.doubler.copyWith(rateHz: v))),
              vm.commitFx,
            ),
            _slider(
              'Mix',
              fx.doubler.mix,
              0,
              1,
              '',
              (v) => _setFx(fx.copyWith(doubler: fx.doubler.copyWith(mix: v))),
              vm.commitFx,
            ),
          ],
        ),
        FxKind.limiter => _simpleInsert(
          index,
          'Limiter',
          fx.limiterOn,
          (v) => _setFx(fx.copyWith(limiterOn: v), commit: true),
          [
            _slider(
              'Ceiling',
              fx.limiter.ceilingDb,
              -12,
              0,
              'dB',
              (v) => _setFx(
                fx.copyWith(limiter: fx.limiter.copyWith(ceilingDb: v)),
              ),
              vm.commitFx,
            ),
            _slider(
              'Release',
              fx.limiter.releaseMs,
              1,
              500,
              'ms',
              (v) => _setFx(
                fx.copyWith(limiter: fx.limiter.copyWith(releaseMs: v)),
              ),
              vm.commitFx,
            ),
          ],
        ),
        // Reverb is a normal series insert: positionable, collapsible, per-take.
        FxKind.reverb => _simpleInsert(
          index,
          'Reverb',
          fx.reverbOn,
          (v) => _setFx(fx.copyWith(reverbOn: v), commit: true),
          [
            Padding(
              padding: const EdgeInsets.symmetric(vertical: 4),
              child: Row(
                children: [
                  CapLabel('Space'),
                  const Spacer(),
                  _Dropdown(
                    hint: 'Space…',
                    items: [for (final p in FxPresets.reverb) p.name],
                    onSelected: (name) {
                      final preset = FxPresets.reverb.firstWhere(
                        (p) => p.name == name,
                      );
                      _setFx(fx.copyWith(reverb: preset.reverb), commit: true);
                    },
                  ),
                ],
              ),
            ),
            _slider(
              'Size',
              fx.reverb.roomSize,
              0,
              1,
              '',
              (v) =>
                  _setFx(fx.copyWith(reverb: fx.reverb.copyWith(roomSize: v))),
              vm.commitFx,
            ),
            _slider(
              'Damping',
              fx.reverb.damping,
              0,
              1,
              '',
              (v) =>
                  _setFx(fx.copyWith(reverb: fx.reverb.copyWith(damping: v))),
              vm.commitFx,
            ),
            _slider(
              'Mix',
              fx.reverb.wet,
              0,
              1,
              '',
              (v) => _setFx(fx.copyWith(reverb: fx.reverb.copyWith(wet: v))),
              vm.commitFx,
            ),
          ],
        ),
      },
    );
  }

  /// A generic insert card: header + (when on) a list of param sliders.
  Widget _simpleInsert(
    int index,
    String title,
    bool on,
    ValueChanged<bool> onToggle,
    List<Widget> body,
  ) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        _insertHeader(index, title, on, onToggle),
        if (on) ...[const SizedBox(height: 6), ...body],
      ],
    );
  }

  /// An insert header: a drag handle, the title, and an on/off toggle.
  Widget _insertHeader(
    int index,
    String title,
    bool on,
    ValueChanged<bool> onToggle,
  ) {
    return Row(
      children: [
        ReorderableDragStartListener(
          index: index,
          child: const MouseRegion(
            cursor: SystemMouseCursors.grab,
            child: Padding(
              padding: EdgeInsets.only(right: 6),
              child: Icon(
                Icons.drag_indicator,
                size: 16,
                color: Studio.textFaint,
              ),
            ),
          ),
        ),
        Text(
          title.toUpperCase(),
          style: TextStyle(
            color: on ? Studio.textPrimary : Studio.textSecondary,
            fontSize: 10.5,
            fontWeight: FontWeight.w700,
            letterSpacing: 0.8,
          ),
        ),
        const Spacer(),
        _Toggle(on: on, onChanged: onToggle),
      ],
    );
  }

  @override
  Widget build(BuildContext context) {
    final fx = _fx;
    return ScrollConfiguration(
      behavior: ScrollConfiguration.of(context).copyWith(scrollbars: false),
      child: ListView(
        padding: const EdgeInsets.fromLTRB(2, 4, 2, 16),
        children: [
          _presetRow(fx),
          const SizedBox(height: 14),
          // Insert chain — drag a handle to reorder the signal flow; each insert
          // collapses to its header when off.
          _InsertChain(
            order: fx.order,
            cardBuilder: (i, kind) => _insertCard(i, kind, fx),
            onReorder: (order) {
              if (_isBus) {
                vm.mixer.reorderBusInserts(bus!.id, order);
              } else {
                vm.mixer.reorderTrackInserts(track!.id, order);
              }
            },
          ),
          const SizedBox(height: 4),
        ],
      ),
    );
  }

  Widget _presetRow(TrackFx fx) {
    return Row(
      children: [
        CapLabel('Preset'),
        const Spacer(),
        _Dropdown(
          hint: 'Choose…',
          menuWidth: 280,
          items: [for (final p in FxPresets.vocal) p.name],
          subtitles: [for (final p in FxPresets.vocal) p.description],
          onSelected: (name) {
            final preset = FxPresets.vocal.firstWhere((p) => p.name == name);
            // A preset replaces the whole insert chain (tone), but keeps the
            // user's custom plugin order. Aux sends / output routing live on the
            // track, not in TrackFx, so they're untouched.
            _setFx(preset.fx.copyWith(order: fx.order), commit: true);
          },
        ),
      ],
    );
  }

  Widget _slider(
    String label,
    double value,
    double min,
    double max,
    String unit,
    ValueChanged<double> onChanged,
    VoidCallback onCommit,
  ) {
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 4),
      child: Row(
        children: [
          SizedBox(
            width: 64,
            child: Text(
              label,
              style: const TextStyle(color: Studio.textFaint, fontSize: 10.5),
            ),
          ),
          Expanded(
            child: _FxSlider(
              value: value,
              min: min,
              max: max,
              onChanged: onChanged,
              onCommit: onCommit,
            ),
          ),
          const SizedBox(width: 8),
          FixedWidthReadout(
            _fmtVal(value, unit),
            width: 42,
            style: const TextStyle(color: Studio.textSecondary, fontSize: 10.5),
          ),
        ],
      ),
    );
  }
}

/// A reorderable insert chain that owns the order LOCALLY, so the drag/drop
/// animation runs entirely in this widget and is never interrupted by the global
/// ViewModel rebuild (which fires from persistence + the engine graph rebuild).
/// The reorder is reflected instantly; [onReorder] pushes it to the model async.
class _InsertChain extends StatefulWidget {
  const _InsertChain({
    required this.order,
    required this.cardBuilder,
    required this.onReorder,
  });

  final List<FxKind> order;
  final Widget Function(int index, FxKind kind) cardBuilder;
  final void Function(List<FxKind> order) onReorder;

  @override
  State<_InsertChain> createState() => _InsertChainState();
}

class _InsertChainState extends State<_InsertChain> {
  late List<FxKind> _order = [...widget.order];

  @override
  void didUpdateWidget(_InsertChain old) {
    super.didUpdateWidget(old);
    // Adopt an externally-changed order (e.g. a different take/bus selected),
    // but ignore the echo of our own reorder (which already matches).
    if (!listEquals(_order, widget.order)) {
      _order = [...widget.order];
    }
  }

  void _onReorder(int oldIndex, int newIndex) {
    setState(() {
      if (newIndex > oldIndex) newIndex -= 1;
      _order.insert(newIndex, _order.removeAt(oldIndex));
    });
    widget.onReorder(_order);
  }

  @override
  Widget build(BuildContext context) {
    return ReorderableListView(
      shrinkWrap: true,
      physics: const NeverScrollableScrollPhysics(),
      buildDefaultDragHandles: false,
      proxyDecorator: (child, index, animation) =>
          Material(color: Studio.surface, child: child),
      onReorder: _onReorder,
      children: [
        for (var i = 0; i < _order.length; i++)
          widget.cardBuilder(i, _order[i]),
      ],
    );
  }
}

String _fmtVal(double v, String unit) {
  switch (unit) {
    case ':1':
      return '${v.toStringAsFixed(1)}:1';
    case 'dB':
      return '${v.round()} dB';
    case 'Hz':
      return v >= 1000
          ? '${(v / 1000).toStringAsFixed(1)}k'
          : '${v.round()} Hz';
    case 'ms':
      return '${v.round()} ms';
    default:
      return '${(v * 100).round()}%';
  }
}

/// A compact studio-style on/off switch.
class _Toggle extends StatelessWidget {
  const _Toggle({required this.on, required this.onChanged});

  final bool on;
  final ValueChanged<bool> onChanged;

  @override
  Widget build(BuildContext context) {
    return GestureDetector(
      behavior: HitTestBehavior.opaque,
      onTap: () => onChanged(!on),
      child: AnimatedContainer(
        duration: const Duration(milliseconds: 130),
        width: 34,
        height: 18,
        decoration: BoxDecoration(
          color: on ? Studio.accent : Studio.border,
          borderRadius: BorderRadius.circular(9),
        ),
        child: AnimatedAlign(
          duration: const Duration(milliseconds: 130),
          curve: Curves.easeOut,
          alignment: on ? Alignment.centerRight : Alignment.centerLeft,
          child: Padding(
            padding: const EdgeInsets.all(2.5),
            child: Container(
              width: 13,
              height: 13,
              decoration: BoxDecoration(
                color: on ? Colors.white : Studio.textSecondary,
                shape: BoxShape.circle,
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// A clean minimal slider: a thin track with an accent fill + small thumb; tap or drag
/// anywhere on it to set the value.
class _FxSlider extends StatefulWidget {
  const _FxSlider({
    required this.value,
    required this.min,
    required this.max,
    required this.onChanged,
    required this.onCommit,
  });

  final double value;
  final double min;
  final double max;
  final ValueChanged<double> onChanged;
  final VoidCallback onCommit;

  @override
  State<_FxSlider> createState() => _FxSliderState();
}

class _FxSliderState extends State<_FxSlider> {
  /// A press or pan that set the value and owes its [_FxSlider.onCommit].
  bool _active = false;

  /// A pan the press turned into: its end commits.
  bool _panning = false;

  void _commit() {
    if (!_active) return;
    _active = false;
    widget.onCommit();
  }

  @override
  void dispose() {
    // Unmounted mid-gesture (the selection changed, the inspector collapsed):
    // a disposed recognizer fires no end, so the value already live in the
    // model was never written. (onCommit only persists the model, which is
    // safe from here.)
    _commit();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return LayoutBuilder(
      builder: (context, c) {
        final w = c.maxWidth;
        void update(double dx) {
          _active = true;
          widget.onChanged(
            widget.min + (dx / w).clamp(0.0, 1.0) * (widget.max - widget.min),
          );
        }

        // The pointer gone (up, or cancelled by a dialog opening while the
        // button was held), a press that set the value commits it, unless a
        // pan owns it (its end commits, a cancelled one too). Judged on the
        // pointer, not the tap: the tap is rejected as soon as it moves past
        // its slop, before a pan has won, and a press the panel's scroll took
        // ends no gesture of this one at all.
        return Listener(
          onPointerUp: (_) => _panning ? null : _commit(),
          onPointerCancel: (_) => _panning ? null : _commit(),
          child: GestureDetector(
            behavior: HitTestBehavior.opaque,
            onTapDown: (d) => update(d.localPosition.dx),
            onTapUp: (_) => _commit(),
            onPanStart: (_) => _panning = true,
            onPanUpdate: (d) => update(d.localPosition.dx),
            onPanEnd: (_) {
              _panning = false;
              _commit();
            },
            child: SizedBox(
              height: 20,
              child: CustomPaint(
                painter: _SliderPainter(
                  ((widget.value - widget.min) / (widget.max - widget.min))
                      .clamp(0.0, 1.0),
                ),
              ),
            ),
          ),
        );
      },
    );
  }
}

class _SliderPainter extends CustomPainter {
  _SliderPainter(this.t);
  final double t;

  @override
  void paint(Canvas canvas, Size size) {
    final cy = size.height / 2;
    final r = const Radius.circular(1.5);
    canvas.drawRRect(
      RRect.fromRectAndRadius(Rect.fromLTWH(0, cy - 1.5, size.width, 3), r),
      Paint()..color = Studio.border,
    );
    final fillW = (size.width * t).clamp(0.0, size.width);
    if (fillW > 0) {
      canvas.drawRRect(
        RRect.fromRectAndRadius(Rect.fromLTWH(0, cy - 1.5, fillW, 3), r),
        Paint()..color = Studio.accent,
      );
    }
    final thumbX = (size.width * t).clamp(6.0, size.width - 6);
    canvas.drawCircle(Offset(thumbX, cy), 6, Paint()..color = Studio.accent);
    canvas.drawCircle(
      Offset(thumbX, cy),
      6,
      Paint()
        ..color = Studio.surface
        ..style = PaintingStyle.stroke
        ..strokeWidth = 2,
    );
  }

  @override
  bool shouldRepaint(_SliderPainter old) => old.t != t;
}

/// Interactive EQ: a live input/output spectrum analyzer behind the combined
/// response curve, plus a draggable dot per band (x = freq, y = gain).
class _EqGraph extends StatefulWidget {
  const _EqGraph({
    required this.inputColumn,
    this.wake,
    required this.bands,
    required this.on,
    required this.onChanged,
    required this.onCommit,
  });

  /// Pre-EQ input spectrum source (null → draw the response curve only).
  final ({Float32List col, double binHz})? Function() inputColumn;

  /// Wakes the idle analyzer (see [_EqSpectrumLayer.wake]).
  final Listenable? wake;
  final List<EqBand> bands;
  final bool on;
  final void Function(int index, EqBand band) onChanged;
  final VoidCallback onCommit;

  @override
  State<_EqGraph> createState() => _EqGraphState();
}

class _EqGraphState extends State<_EqGraph> {
  // Survives rebuilds (live FX edits call notifyListeners mid-drag).
  int? _dragging;
  Timer? _qCommit;

  int _nearestBand(double x, double w) {
    var best = 0;
    var bestDx = double.infinity;
    for (var i = 0; i < widget.bands.length; i++) {
      final dx = (_freqToX(widget.bands[i].freq, w) - x).abs();
      if (dx < bestDx) {
        bestDx = dx;
        best = i;
      }
    }
    return best;
  }

  @override
  void dispose() {
    // Flush, don't drop, a wheel Q edit still inside its debounce: the edit is
    // already live in the model, so changing the selection, collapsing the
    // inspector or leaving within 400 ms of the last tick would otherwise keep
    // it in memory and never write it. (onCommit only persists the model,
    // which is safe from here.)
    // A band drag still in flight is the same: a disposed recognizer fires no
    // end, so the band it moved was never written either.
    final debounced = _qCommit?.isActive ?? false;
    _qCommit?.cancel();
    if (debounced || _dragging != null) {
      _dragging = null;
      widget.onCommit();
    }
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return LayoutBuilder(
      builder: (context, c) {
        final w = c.maxWidth;
        const h = 120.0;
        return Listener(
          // Scroll over a band to widen / narrow its Q (bandwidth). Register with the signal
          // resolver so the scroll is consumed here and doesn't also scroll the FX list.
          onPointerSignal: (signal) {
            if (signal is PointerScrollEvent) {
              GestureBinding.instance.pointerSignalResolver.register(signal, (
                e,
              ) {
                final s = e as PointerScrollEvent;
                final i = _nearestBand(s.localPosition.dx, w);
                final b = widget.bands[i];
                final q = (b.q * math.exp(-s.scrollDelta.dy * 0.0022)).clamp(
                  0.3,
                  14.0,
                );
                widget.onChanged(i, b.copyWith(q: q, enabled: true));
                _qCommit?.cancel();
                _qCommit = Timer(
                  const Duration(milliseconds: 400),
                  widget.onCommit,
                );
              });
            }
          },
          child: GestureDetector(
            behavior: HitTestBehavior.opaque,
            onPanStart: (d) => _dragging = _nearestBand(d.localPosition.dx, w),
            onPanUpdate: (d) {
              final i = _dragging;
              if (i == null) return;
              final f = _xToFreq(
                d.localPosition.dx,
                w,
              ).clamp(_eqMinHz, _graphSr * 0.45);
              final g = _yToDb(d.localPosition.dy, h);
              widget.onChanged(
                i,
                widget.bands[i].copyWith(freq: f, gainDb: g, enabled: true),
              );
            },
            onPanEnd: (_) {
              _dragging = null;
              widget.onCommit();
            },
            child: SizedBox(
              height: h,
              width: w,
              child: Stack(
                children: [
                  // Spectrum + background grid behind the response curve.
                  Positioned.fill(
                    child: _EqSpectrumLayer(
                      inputColumn: widget.inputColumn,
                      wake: widget.wake,
                      bands: widget.bands,
                      on: widget.on,
                    ),
                  ),
                  Positioned.fill(
                    child: CustomPaint(
                      painter: _EqPainter(widget.bands, widget.on),
                    ),
                  ),
                  // Live freq/gain readout for the band being dragged.
                  if (_dragging != null && _dragging! < widget.bands.length)
                    Positioned(
                      left: 8,
                      top: 6,
                      child: _ReadoutPill(band: widget.bands[_dragging!]),
                    ),
                ],
              ),
            ),
          ),
        );
      },
    );
  }
}

String _fmtHz(double f) =>
    f >= 1000 ? '${(f / 1000).toStringAsFixed(1)} kHz' : '${f.round()} Hz';

/// A small freq · gain readout shown while a band is dragged.
class _ReadoutPill extends StatelessWidget {
  const _ReadoutPill({required this.band});
  final EqBand band;

  @override
  Widget build(BuildContext context) {
    final g = band.gainDb;
    final gain = '${g >= 0 ? '+' : '−'}${g.abs().toStringAsFixed(1)} dB';
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 7, vertical: 3),
      decoration: BoxDecoration(
        color: Studio.surfaceRaised.withValues(alpha: 0.92),
        borderRadius: BorderRadius.circular(5),
        border: Border.all(color: Studio.border),
      ),
      child: Text(
        '${_fmtHz(band.freq)}  ·  $gain',
        style: const TextStyle(
          color: Studio.textSecondary,
          fontSize: 10,
          fontWeight: FontWeight.w600,
        ),
      ),
    );
  }
}

/// Per-frame spectrum state, mutated in place by the ticker and read live by
/// [_SpectrumPainter] (the painter holds this same instance, so it always sees
/// the latest values without a widget rebuild).
class _SpectrumData {
  /// Smoothed input level (dB, display-tilted) per render point.
  final Float32List pre = Float32List(_spectrumPoints)
    ..fillRange(0, _spectrumPoints, -200);

  /// Adaptive top-of-scale reference (dB) — rises fast to peaks, falls slowly,
  /// so the display fills the box regardless of the take's absolute level.
  double ceil = -30;

  /// True once any real input has been seen, so an idle EQ stays blank.
  bool hasSignal = false;
}

/// Drives the live spectrum: a ticker samples the playhead's input column each
/// frame, octave-smooths + tilts it, applies analyzer ballistics (instant
/// attack, slow release), and repaints [_SpectrumPainter].
///
/// The ticker stops once a frame changes nothing (the ballistics have settled
/// on the input), so a paused transport no longer produces frames forever —
/// it ticked at the display rate for as long as a take with the EQ on was
/// selected. [wake] (the playhead, and the analysis caches a newly computed
/// input spectrum arrives in) and any rebuild — which is how a band edit
/// arrives — start it again.
class _EqSpectrumLayer extends StatefulWidget {
  const _EqSpectrumLayer({
    required this.inputColumn,
    this.wake,
    required this.bands,
    required this.on,
  });

  final ({Float32List col, double binHz})? Function() inputColumn;

  /// Changes whenever the input may have: the playhead and the analysis.
  final Listenable? wake;
  final List<EqBand> bands;
  final bool on;

  @override
  State<_EqSpectrumLayer> createState() => _EqSpectrumLayerState();
}

class _EqSpectrumLayerState extends State<_EqSpectrumLayer>
    with SingleTickerProviderStateMixin {
  static const double _tiltDbPerOct = 4.5; // lift highs so pink-ish reads flat
  static const double _floorDb = -90;
  static const double _releaseDbPerSec = 34; // spectrum fall speed
  static const double _ceilFallPerSec = 5;

  final _SpectrumData _data = _SpectrumData();
  final ValueNotifier<int> _repaint = ValueNotifier(0);
  late final Ticker _ticker;
  Duration _last = Duration.zero;

  @override
  void initState() {
    super.initState();
    _ticker = createTicker(_tick)..start();
    widget.wake?.addListener(_wake);
  }

  @override
  void didUpdateWidget(_EqSpectrumLayer old) {
    super.didUpdateWidget(old);
    if (!identical(old.wake, widget.wake)) {
      old.wake?.removeListener(_wake);
      widget.wake?.addListener(_wake);
    }
    _wake();
  }

  @override
  void dispose() {
    widget.wake?.removeListener(_wake);
    _ticker.dispose();
    _repaint.dispose();
    super.dispose();
  }

  void _wake() {
    if (_ticker.isActive) return;
    // A restarted ticker counts from zero again: start the frame clock over
    // as at the first start.
    _last = Duration.zero;
    _ticker.start();
  }

  void _tick(Duration now) {
    final dt = _last == Duration.zero
        ? 0.016
        : ((now - _last).inMicroseconds / 1e6).clamp(0.0, 0.1);
    _last = now;

    final input = widget.inputColumn();
    final target = Float32List(_spectrumPoints);
    var changed = false;
    if (input == null) {
      target.fillRange(0, _spectrumPoints, _floorDb);
    } else {
      if (!_data.hasSignal) changed = true;
      _data.hasSignal = true;
      final col = input.col;
      final binHz = input.binHz;
      final bins = col.length;
      final ratio = _eqMaxHz / _eqMinHz;
      for (var i = 0; i < _spectrumPoints; i++) {
        // Average power across the bins spanning this point's octave slice.
        final fLo = _eqMinHz * math.pow(ratio, i / _spectrumPoints).toDouble();
        final fHi =
            _eqMinHz * math.pow(ratio, (i + 1) / _spectrumPoints).toDouble();
        var bLo = (fLo / binHz).floor();
        var bHi = (fHi / binHz).ceil();
        if (bLo < 0) bLo = 0;
        if (bHi <= bLo) bHi = bLo + 1;
        if (bHi > bins) bHi = bins;
        if (bLo >= bins) {
          target[i] = _floorDb;
          continue;
        }
        var power = 0.0;
        var n = 0;
        for (var b = bLo; b < bHi; b++) {
          power += math.pow(10.0, col[b] / 10.0);
          n++;
        }
        var db = n == 0 ? _floorDb : 10.0 * math.log(power / n) / math.ln10;
        db +=
            _tiltDbPerOct * (math.log(_spectrumPointFreq(i) / 1000) / math.ln2);
        target[i] = db < _floorDb ? _floorDb : db;
      }
    }

    // Ballistics: instant attack, linear release; track the running peak.
    final fall = _releaseDbPerSec * dt;
    var maxDb = _floorDb;
    for (var i = 0; i < _spectrumPoints; i++) {
      final tv = target[i];
      final was = _data.pre[i];
      _data.pre[i] = tv >= was ? tv : math.max(tv, was - fall);
      if (_data.pre[i] != was) changed = true;
      if (_data.pre[i] > maxDb) maxDb = _data.pre[i];
    }
    // Adapt the top-of-scale only while there's real input; otherwise hold it so
    // the decaying spectrum falls to the bottom (not into the middle of the box).
    if (input != null) {
      final desiredCeil = maxDb + 6;
      final was = _data.ceil;
      _data.ceil = desiredCeil > was
          ? desiredCeil
          : math.max(desiredCeil, was - _ceilFallPerSec * dt);
      if (_data.ceil != was) changed = true;
    }

    _repaint.value++;
    // Settled: with time passing (dt > 0) and nothing moving, every later
    // frame would paint this one again until the input changes.
    if (!changed && dt > 0) _ticker.stop();
  }

  @override
  Widget build(BuildContext context) {
    return CustomPaint(
      painter: _SpectrumPainter(
        data: _data,
        bands: widget.bands,
        on: widget.on,
        repaint: _repaint,
      ),
    );
  }
}

class _SpectrumPainter extends CustomPainter {
  _SpectrumPainter({
    required this.data,
    required this.bands,
    required this.on,
    required this.repaint,
  }) : super(repaint: repaint);

  final _SpectrumData data;
  final List<EqBand> bands;
  final bool on;
  final Listenable repaint;

  @override
  void paint(Canvas canvas, Size size) {
    final w = size.width, h = size.height;
    final rrect = RRect.fromRectAndRadius(
      Offset.zero & size,
      const Radius.circular(8),
    );

    // Background + log frequency grid (moved here so it sits behind the
    // spectrum): faint minor lines, brighter labeled decades.
    canvas.drawRRect(rrect, Paint()..color = Studio.inset);
    canvas.save();
    canvas.clipRRect(rrect);
    final minor = Paint()..color = Studio.border.withValues(alpha: 0.28);
    for (final f in const [
      30.0,
      50.0,
      200.0,
      300.0,
      500.0,
      2000.0,
      3000.0,
      5000.0,
      20000.0,
    ]) {
      final x = _freqToX(f, w);
      canvas.drawLine(Offset(x, 0), Offset(x, h), minor);
    }
    final major = Paint()..color = Studio.border.withValues(alpha: 0.6);
    for (final entry in const [
      (100.0, '100'),
      (1000.0, '1k'),
      (10000.0, '10k'),
    ]) {
      final x = _freqToX(entry.$1, w);
      canvas.drawLine(Offset(x, 0), Offset(x, h), major);
      final tp = TextPainter(
        text: TextSpan(
          text: entry.$2,
          style: const TextStyle(color: Studio.textFaint, fontSize: 8.5),
        ),
        textDirection: TextDirection.ltr,
      )..layout();
      tp.paint(canvas, Offset(x + 3, h - tp.height - 2));
    }

    if (data.hasSignal) {
      const top = 0.10; // ceiling maps near the top of the box
      const range = 80.0; // dB shown vertically
      double yOf(double db) {
        final t = ((db - (data.ceil - range)) / range).clamp(0.0, 1.0);
        return h - t * (h * (1 - top));
      }

      final pre = <Offset>[];
      final post = <Offset>[];
      for (var i = 0; i < _spectrumPoints; i++) {
        final f = _spectrumPointFreq(i);
        final x = _freqToX(f, w);
        final preDb = data.pre[i];
        pre.add(Offset(x, yOf(preDb)));
        // Output = input × EQ, i.e. input dB + response dB.
        post.add(Offset(x, yOf(preDb + _totalDb(bands, on, f))));
      }

      void fillUnder(List<Offset> pts, Color color) {
        final path = Path()..addPolygon(pts, false);
        path
          ..lineTo(pts.last.dx, h)
          ..lineTo(pts.first.dx, h)
          ..close();
        canvas.drawPath(path, Paint()..color = color);
      }

      void stroke(List<Offset> pts, Color color, double width) {
        canvas.drawPath(
          Path()..addPolygon(pts, false),
          Paint()
            ..color = color
            ..style = PaintingStyle.stroke
            ..strokeWidth = width
            ..strokeJoin = StrokeJoin.round,
        );
      }

      // Input (pre): dim grey fill + faint line. Output (post): brighter.
      fillUnder(pre, Colors.white.withValues(alpha: 0.045));
      stroke(pre, Colors.white.withValues(alpha: 0.12), 1);
      fillUnder(post, Colors.white.withValues(alpha: 0.10));
      stroke(post, Colors.white.withValues(alpha: 0.42), 1.2);
    }

    canvas.restore();
  }

  @override
  bool shouldRepaint(_SpectrumPainter old) =>
      old.on != on || !identical(old.bands, bands);
}

class _EqPainter extends CustomPainter {
  _EqPainter(this.bands, this.on);
  final List<EqBand> bands;
  final bool on;

  @override
  void paint(Canvas canvas, Size size) {
    final w = size.width, h = size.height;
    // Background, grid and spectrum are drawn by the layer behind this; here we
    // only draw the 0 dB reference line, the response curve and the handles.
    canvas.drawLine(
      Offset(0, h / 2),
      Offset(w, h / 2),
      Paint()
        ..color = Studio.border
        ..strokeWidth = 1,
    );

    // Response curve + a soft fill to the 0 dB line.
    final cy = h / 2;
    final pts = <Offset>[];
    for (var px = 0; px <= w; px += 2) {
      final f = _xToFreq(px.toDouble(), w);
      pts.add(Offset(px.toDouble(), _dbToY(_totalDb(bands, on, f), h)));
    }

    if (on) {
      final fill = Path()
        ..addPolygon(pts, false)
        ..lineTo(w, cy)
        ..lineTo(0, cy)
        ..close();
      canvas.drawPath(
        fill,
        Paint()..color = Studio.accent.withValues(alpha: 0.12),
      );
    }

    canvas.drawPath(
      Path()..addPolygon(pts, false),
      Paint()
        ..color = on ? Studio.accent : Studio.textFaint.withValues(alpha: 0.5)
        ..style = PaintingStyle.stroke
        ..strokeWidth = 2.5
        ..strokeJoin = StrokeJoin.round,
    );

    // Band handles.
    for (final b in bands) {
      final x = _freqToX(b.freq.clamp(_eqMinHz, _eqMaxHz), w);
      final y = _dbToY(b.gainDb, h);
      canvas.drawCircle(
        Offset(x, y),
        6,
        Paint()..color = on ? Studio.accent : Studio.textFaint,
      );
      canvas.drawCircle(
        Offset(x, y),
        6,
        Paint()
          ..color = Studio.surface
          ..style = PaintingStyle.stroke
          ..strokeWidth = 2,
      );
      canvas.drawCircle(
        Offset(x, y),
        2.4,
        Paint()..color = Colors.white.withValues(alpha: on ? 0.9 : 0.35),
      );
    }
  }

  @override
  bool shouldRepaint(_EqPainter old) =>
      old.on != on || !identical(old.bands, bands);
}

class _Dropdown extends StatelessWidget {
  const _Dropdown({
    required this.hint,
    required this.items,
    required this.onSelected,
    this.subtitles,
    this.menuWidth = 184,
  });
  final String hint;
  final List<String> items;
  final ValueChanged<String> onSelected;

  /// Optional per-item descriptions, parallel to [items] — shown under each label.
  final List<String>? subtitles;
  final double menuWidth;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTap: () => showStudioMenu(context, menuAnchorBelow(context), [
          for (var i = 0; i < items.length; i++)
            StudioMenuItem(
              label: items[i],
              subtitle: (subtitles != null && i < subtitles!.length)
                  ? subtitles![i]
                  : null,
              onTap: () => onSelected(items[i]),
            ),
        ], width: menuWidth),
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 5),
          decoration: BoxDecoration(
            color: Studio.inset,
            borderRadius: BorderRadius.circular(6),
            border: Border.all(color: Studio.border),
          ),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Text(
                hint,
                style: const TextStyle(
                  color: Studio.textSecondary,
                  fontSize: 11,
                ),
              ),
              const SizedBox(width: 4),
              const Icon(Icons.expand_more, size: 14, color: Studio.textFaint),
            ],
          ),
        ),
      ),
    );
  }
}

/// A live gain-reduction meter for a vocal compressor — polls the engine and shows the
/// reduction as a bar (accent → red as it deepens) with a falling peak tick.
///
/// Polling stops once the reading has held still for [_GrMeterState._idlePolls]
/// polls (it rebuilt at 20 Hz for as long as the compressor was shown, even
/// with nothing playing), and [wake] or a rebuild starts it again.
class _GrMeter extends StatefulWidget {
  const _GrMeter({required this.poll, this.wake});

  /// Polls the current gain reduction (dB, >= 0) of the target compressor.
  final double Function() poll;

  /// Changes whenever audio may be running through the compressor: the
  /// playhead.
  final Listenable? wake;

  @override
  State<_GrMeter> createState() => _GrMeterState();
}

class _GrMeterState extends State<_GrMeter> {
  /// Unchanged polls (at 50 ms) before polling stops.
  static const int _idlePolls = 10;

  Timer? _timer;
  int _unchanged = 0;
  double _gr = 0;
  double _peak = 0;

  @override
  void initState() {
    super.initState();
    _start();
    widget.wake?.addListener(_start);
  }

  @override
  void didUpdateWidget(_GrMeter old) {
    super.didUpdateWidget(old);
    if (!identical(old.wake, widget.wake)) {
      old.wake?.removeListener(_start);
      widget.wake?.addListener(_start);
    }
    _start();
  }

  @override
  void dispose() {
    widget.wake?.removeListener(_start);
    _timer?.cancel();
    super.dispose();
  }

  void _start() {
    _unchanged = 0;
    _timer ??= Timer.periodic(const Duration(milliseconds: 50), (_) => _poll());
  }

  void _poll() {
    final v = widget.poll();
    if (!mounted) return;
    final peak = v > _peak ? v : math.max(v, _peak - 0.8);
    if (v == _gr && peak == _peak) {
      if (++_unchanged >= _idlePolls) {
        _timer?.cancel();
        _timer = null;
      }
      return;
    }
    _unchanged = 0;
    setState(() {
      _gr = v;
      _peak = peak;
    });
  }

  @override
  Widget build(BuildContext context) {
    const maxGr = 18.0;
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 4),
      child: Row(
        children: [
          const SizedBox(
            width: 64,
            child: Text(
              'GR',
              style: TextStyle(color: Studio.textFaint, fontSize: 10.5),
            ),
          ),
          Expanded(
            child: SizedBox(
              height: 8,
              child: CustomPaint(
                painter: _GrPainter(_gr / maxGr, _peak / maxGr),
              ),
            ),
          ),
          const SizedBox(width: 8),
          SizedBox(
            width: 42,
            child: Text(
              _gr < 0.05 ? '0.0' : '-${_gr.toStringAsFixed(1)}',
              textAlign: TextAlign.right,
              style: const TextStyle(
                color: Studio.textSecondary,
                fontSize: 10.5,
              ),
            ),
          ),
        ],
      ),
    );
  }
}

class _GrPainter extends CustomPainter {
  _GrPainter(this.t, this.peak);
  final double t;
  final double peak;

  @override
  void paint(Canvas canvas, Size size) {
    final cy = size.height / 2;
    final r = const Radius.circular(1.5);
    canvas.drawRRect(
      RRect.fromRectAndRadius(Rect.fromLTWH(0, cy - 1.5, size.width, 3), r),
      Paint()..color = Studio.border,
    );
    final tc = t.clamp(0.0, 1.0);
    if (tc > 0) {
      final color = Color.lerp(
        Studio.accent,
        Studio.record,
        (tc * 1.6).clamp(0.0, 1.0),
      )!;
      canvas.drawRRect(
        RRect.fromRectAndRadius(
          Rect.fromLTWH(0, cy - 1.5, size.width * tc, 3),
          r,
        ),
        Paint()..color = color,
      );
    }
    final pc = peak.clamp(0.0, 1.0);
    if (pc > 0.01) {
      final x = (size.width * pc).clamp(1.0, size.width - 1);
      canvas.drawRect(
        Rect.fromLTWH(x - 1, cy - 3.5, 2, 7),
        Paint()..color = Studio.textSecondary,
      );
    }
  }

  @override
  bool shouldRepaint(_GrPainter old) => old.t != t || old.peak != peak;
}
