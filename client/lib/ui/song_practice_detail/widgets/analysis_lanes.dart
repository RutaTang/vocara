import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:forui/forui.dart';

import '../../../domain/models/audio_track.dart';
import '../../../domain/models/practice_note.dart';
import '../../../domain/models/spectrogram.dart';
import '../../../domain/models/technique_marker.dart';
import '../../../domain/models/voice_metrics.dart';
import '../../core/themes/studio.dart';
import '../../core/widgets/spectrogram_painter.dart';
import '../../core/widgets/studio_hint.dart';
import '../../core/widgets/waveform_painter.dart';
import '../view_model/song_practice_detail_view_model.dart';
import 'pitch_lane.dart';
import 'track_row.dart';

const Map<AnalysisLane, String> _laneLabels = {
  AnalysisLane.pitch: 'Pitch',
  AnalysisLane.loudness: 'Loudness',
  AnalysisLane.spectrogram: 'Spectrogram',
  AnalysisLane.power: 'Power',
  AnalysisLane.notes: 'Notes',
  AnalysisLane.technique: 'Techniques',
};

/// The fold-out analysis area under a vocal take: a toolbar to toggle which
/// sub-lanes show, then those lanes (pitch / spectrogram / power / notes), all
/// sharing the timeline + header-column width with the parent track.
class AnalysisLanes extends StatelessWidget {
  const AnalysisLanes({
    required this.vm,
    required this.track,
    required this.timelineDurationMs,
    required this.onAddNote,
    required this.onEditNote,
    required this.onAddTechnique,
    required this.onEditTechnique,
    super.key,
  });

  final SongPracticeDetailViewModel vm;
  final AudioTrack track;
  final int timelineDurationMs;
  final void Function(int startMs, int? endMs) onAddNote;
  final ValueChanged<PracticeNote> onEditNote;
  final void Function(int startMs, int? endMs) onAddTechnique;
  final ValueChanged<TechniqueMarker> onEditTechnique;

  @override
  Widget build(BuildContext context) {
    final id = track.id;
    final lanes = vm.analysis.lanesFor(id);
    final viewStart = vm.playback.viewStartMs;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        _LaneToolbar(
          active: lanes,
          onToggle: (l) => vm.analysis.toggleLane(id, l),
        ),
        if (lanes.contains(AnalysisLane.pitch))
          PitchLane(
            track: track,
            timelineDurationMs: timelineDurationMs,
            viewStartMs: viewStart,
            position: vm.position,
            headerWidth: kTrackHeaderW,
            loading: vm.analysis.metricsLoadingFor(id),
            metrics: vm.analysis.metricsFor(id),
          ),
        if (lanes.contains(AnalysisLane.loudness))
          LoudnessLane(
            track: track,
            timelineDurationMs: timelineDurationMs,
            viewStartMs: viewStart,
            position: vm.position,
            loading: vm.analysis.metricsLoadingFor(id),
            metrics: vm.analysis.metricsFor(id),
          ),
        if (lanes.contains(AnalysisLane.spectrogram))
          SpectrogramLane(
            track: track,
            timelineDurationMs: timelineDurationMs,
            viewStartMs: viewStart,
            position: vm.position,
            loading: vm.analysis.spectrogramLoadingFor(id),
            spectrogram: vm.analysis.spectrogramFor(id),
          ),
        if (lanes.contains(AnalysisLane.power))
          PowerLane(
            track: track,
            position: vm.position,
            loading: vm.analysis.spectrogramLoadingFor(id),
            spectrogram: vm.analysis.spectrogramFor(id),
            metrics: vm.analysis.metricsFor(id),
          ),
        if (lanes.contains(AnalysisLane.notes))
          NotesLane(
            track: track,
            timelineDurationMs: timelineDurationMs,
            viewStartMs: viewStart,
            position: vm.position,
            onAddNote: onAddNote,
            onEditNote: onEditNote,
          ),
        if (lanes.contains(AnalysisLane.technique))
          TechniqueLane(
            track: track,
            timelineDurationMs: timelineDurationMs,
            viewStartMs: viewStart,
            position: vm.position,
            onAddTechnique: onAddTechnique,
            onEditTechnique: onEditTechnique,
          ),
      ],
    );
  }
}

/// The lane toggle bar: a chip per [AnalysisLane], lit when that lane is shown.
class _LaneToolbar extends StatelessWidget {
  const _LaneToolbar({required this.active, required this.onToggle});

  final Set<AnalysisLane> active;
  final ValueChanged<AnalysisLane> onToggle;

  @override
  Widget build(BuildContext context) {
    return Container(
      height: 30,
      decoration: const BoxDecoration(
        color: Studio.surface,
        border: Border(bottom: BorderSide(color: Studio.border)),
      ),
      child: Row(
        children: [
          const SizedBox(
            width: kTrackHeaderW,
            child: Padding(
              padding: EdgeInsets.only(left: 30),
              child: Align(
                alignment: Alignment.centerLeft,
                child: Text(
                  'LANES',
                  style: TextStyle(
                    color: Studio.textFaint,
                    fontSize: 9,
                    fontWeight: FontWeight.w700,
                    letterSpacing: 0.8,
                  ),
                ),
              ),
            ),
          ),
          Expanded(
            // Scroll rather than overflow if the center area is dragged narrow.
            child: SingleChildScrollView(
              scrollDirection: Axis.horizontal,
              padding: const EdgeInsets.symmetric(horizontal: 8),
              child: Row(
                children: [
                  for (final l in AnalysisLane.values)
                    Padding(
                      padding: const EdgeInsets.only(right: 6),
                      child: _chip(
                        _laneLabels[l]!,
                        active.contains(l),
                        () => onToggle(l),
                      ),
                    ),
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }

  Widget _chip(String label, bool on, VoidCallback onTap) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onTap,
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 4),
          decoration: BoxDecoration(
            color: on
                ? Studio.accent.withValues(alpha: 0.18)
                : Studio.surfaceRaised,
            borderRadius: BorderRadius.circular(5),
            border: Border.all(
              color: on ? Studio.accent.withValues(alpha: 0.6) : Studio.border,
            ),
          ),
          child: Text(
            label,
            style: TextStyle(
              color: on ? Studio.accentBright : Studio.textSecondary,
              fontSize: 11,
              fontWeight: FontWeight.w600,
            ),
          ),
        ),
      ),
    );
  }
}

/// A take's spectrogram, time-aligned under its waveform.
class SpectrogramLane extends StatelessWidget {
  const SpectrogramLane({
    required this.track,
    required this.timelineDurationMs,
    required this.position,
    required this.loading,
    required this.spectrogram,
    this.viewStartMs = 0,
    super.key,
  });

  final AudioTrack track;
  final int timelineDurationMs;
  final int viewStartMs;
  final ValueListenable<Duration> position;
  final bool loading;
  final Spectrogram? spectrogram;

  @override
  Widget build(BuildContext context) {
    final spec = spectrogram;
    Widget body;
    if (loading) {
      body = const LaneHint('Analyzing…');
    } else if (spec == null) {
      body = const LaneHint('Spectrogram is available for recorded WAV takes.');
    } else {
      body = LayoutBuilder(
        builder: (context, c) {
          final w = c.maxWidth;
          double xOf(int ms) => timelineDurationMs <= 0
              ? 0.0
              : (ms - viewStartMs) / timelineDurationMs * w;
          final startX = xOf(track.startMs);
          final endX = xOf(track.startMs + track.durationMs);
          return Stack(
            children: [
              const Positioned.fill(child: ColoredBox(color: Studio.inset)),
              Positioned(
                left: startX,
                top: 0,
                bottom: 0,
                width: (endX - startX) < 1 ? 1.0 : (endX - startX),
                child: ClipRect(
                  child: Stack(
                    children: [
                      Positioned.fill(
                        child: SpectrogramImage(spectrogram: spec),
                      ),
                      Positioned.fill(
                        child: IgnorePointer(
                          child: CustomPaint(
                            painter: FrequencyGridPainter(maxHz: spec.maxHz),
                          ),
                        ),
                      ),
                    ],
                  ),
                ),
              ),
              Positioned.fill(
                child: PlayheadLayer(
                  position: position,
                  timelineDurationMs: timelineDurationMs,
                  startMs: viewStartMs,
                  color: Studio.playhead,
                ),
              ),
            ],
          );
        },
      );
    }
    return _laneShell('SPECTROGRAM', kPitchLaneHeight, body);
  }
}

/// The instantaneous power spectrum at the playhead (frequency X-axis — not
/// time-aligned, like the dock's), with the take's formants marked.
class PowerLane extends StatelessWidget {
  const PowerLane({
    required this.track,
    required this.position,
    required this.loading,
    required this.spectrogram,
    required this.metrics,
    super.key,
  });

  final AudioTrack track;
  final ValueListenable<Duration> position;
  final bool loading;
  final Spectrogram? spectrogram;
  final VoiceMetrics? metrics;

  @override
  Widget build(BuildContext context) {
    final spec = spectrogram;
    Widget body;
    if (loading) {
      body = const LaneHint('Analyzing…');
    } else if (spec == null) {
      body = const LaneHint(
        'Power spectrum is available for recorded WAV takes.',
      );
    } else {
      body = ColoredBox(
        color: Studio.inset,
        // The spectrum follows the playhead through its painter's repaint
        // listenable, on a layer of its own: a tick repaints just this. A
        // per-tick rebuild here would sit under the timeline's LayoutBuilders
        // and re-lay-out the whole center pane on every frame of playback.
        child: RepaintBoundary(
          child: CustomPaint(
            // Childless CustomPaint defaults to Size.zero and collapses in the
            // lane's loose-height slot — Size.infinite makes it fill instead.
            size: Size.infinite,
            painter: _PlayheadPowerPainter(
              position: position,
              track: track,
              spectrogram: spec,
              metrics: metrics,
            ),
          ),
        ),
      );
    }
    return _laneShell('POWER SPECTRUM', kPitchLaneHeight, body);
  }
}

/// [PowerSpectrumPainter] for the column under the playhead: reads [position]
/// at paint time and repaints whenever it ticks.
class _PlayheadPowerPainter extends CustomPainter {
  _PlayheadPowerPainter({
    required this.position,
    required this.track,
    required this.spectrogram,
    required this.metrics,
  }) : super(repaint: position);

  final ValueListenable<Duration> position;
  final AudioTrack track;
  final Spectrogram spectrogram;
  final VoiceMetrics? metrics;

  @override
  void paint(Canvas canvas, Size size) {
    final ms = position.value.inMilliseconds - track.startMs;
    // Outside the take nothing is playing: paint an empty spectrum
    // rather than the take's first/last frame (a stale edge).
    final inSpan = ms >= 0 && ms <= track.durationMs;
    final seconds = ms / 1000.0;
    final m = metrics;
    var formants = const <double>[];
    if (inSpan && m != null && m.frames > 0) {
      final fr = m.frameAtTime(seconds);
      formants = [m.f1[fr], m.f2[fr], m.f3[fr]];
    }
    PowerSpectrumPainter(
      column: inSpan ? spectrogram.columnAtTime(seconds) : Float32List(0),
      binHz: spectrogram.binHz,
      maxHz: spectrogram.maxHz,
      formants: formants,
    ).paint(canvas, size);
  }

  @override
  bool shouldRepaint(_PlayheadPowerPainter old) =>
      !identical(old.position, position) ||
      old.track != track ||
      old.spectrogram != spectrogram ||
      old.metrics != metrics;
}

/// The take's loudness (RMS dBFS) envelope — the dynamics / phrasing curve,
/// time-aligned under the waveform.
class LoudnessLane extends StatelessWidget {
  const LoudnessLane({
    required this.track,
    required this.timelineDurationMs,
    required this.position,
    required this.loading,
    required this.metrics,
    this.viewStartMs = 0,
    super.key,
  });

  final AudioTrack track;
  final int timelineDurationMs;
  final int viewStartMs;
  final ValueListenable<Duration> position;
  final bool loading;
  final VoiceMetrics? metrics;

  @override
  Widget build(BuildContext context) {
    final m = metrics;
    Widget body;
    if (loading) {
      body = const LaneHint('Analyzing…');
    } else if (m == null || m.frames == 0) {
      body = const LaneHint('Loudness is available for recorded WAV takes.');
    } else {
      body = Stack(
        children: [
          const Positioned.fill(child: ColoredBox(color: Studio.inset)),
          Positioned.fill(
            child: IgnorePointer(
              child: CustomPaint(
                painter: LoudnessLanePainter(
                  metrics: m,
                  timelineDurationMs: timelineDurationMs,
                  startMs: track.startMs,
                  viewStartMs: viewStartMs,
                ),
              ),
            ),
          ),
          Positioned.fill(
            child: PlayheadLayer(
              position: position,
              timelineDurationMs: timelineDurationMs,
              startMs: viewStartMs,
              color: Studio.playhead,
            ),
          ),
        ],
      );
    }
    return _laneShell('LOUDNESS', kPitchLaneHeight, body);
  }
}

/// Draws the loudness envelope as a filled area + top line, auto-ranged to the
/// take's peak over a fixed [_rangeDb] window.
class LoudnessLanePainter extends CustomPainter {
  LoudnessLanePainter({
    required this.metrics,
    required this.timelineDurationMs,
    required this.startMs,
    this.viewStartMs = 0,
  });

  final VoiceMetrics metrics;
  final int timelineDurationMs;
  final int startMs;
  final int viewStartMs;

  static const double _rangeDb = 48;

  @override
  void paint(Canvas canvas, Size size) {
    final dur = timelineDurationMs;
    final frames = metrics.frames;
    if (dur <= 0 || frames == 0) return;
    // Zoomed in, most of the envelope maps outside the box; keep it from
    // painting over the track-header column (see [PitchLanePainter.paint]).
    canvas.clipRect(Offset.zero & size);

    var peak = -1e9;
    for (final v in metrics.loudness) {
      if (v.isFinite && v > peak) peak = v;
    }
    if (peak <= -1e8) return;
    final floor = peak - _rangeDb;

    double xOf(double ms) => (ms - viewStartMs) / dur * size.width;
    double yOf(double db) =>
        (1 - ((db - floor) / _rangeDb).clamp(0.0, 1.0)) * size.height;

    // Segmented paths: a multi-clip take's aggregate metrics carry NaN frames
    // in the gaps between clips, which must break the envelope instead of
    // feeding NaN points into the path. Single-clip loudness is always finite.
    final area = Path();
    final line = Path();
    var started = false;
    var any = false;
    var lastX = 0.0;
    void closeSegment() {
      if (!started) return;
      area
        ..lineTo(lastX, size.height)
        ..close();
      started = false;
    }

    // Only the frames that can reach the (clipped) box: zoomed in on a long
    // take, the path would otherwise run through every frame of it.
    final window = laneVisibleFrames(
      metrics,
      startMs: startMs,
      viewStartMs: viewStartMs,
      durMs: dur,
      width: size.width,
    );
    for (var fr = window.first; fr <= window.last; fr++) {
      final db = metrics.loudness[fr];
      if (!db.isFinite) {
        closeSegment();
        continue;
      }
      final x = xOf(startMs + metrics.timeOfFrame(fr) * 1000);
      final y = yOf(db);
      if (!started) {
        area.moveTo(x, size.height);
        area.lineTo(x, y);
        line.moveTo(x, y);
        started = true;
        any = true;
      } else {
        area.lineTo(x, y);
        line.lineTo(x, y);
      }
      lastX = x;
    }
    if (!any) return;
    closeSegment();
    canvas.drawPath(
      area,
      Paint()..color = Studio.accent.withValues(alpha: 0.18),
    );
    canvas.drawPath(
      line,
      Paint()
        ..color = Studio.accent.withValues(alpha: 0.85)
        ..style = PaintingStyle.stroke
        ..strokeWidth = 1.5,
    );
  }

  @override
  bool shouldRepaint(LoudnessLanePainter old) =>
      old.metrics != metrics ||
      old.timelineDurationMs != timelineDurationMs ||
      old.startMs != startMs ||
      old.viewStartMs != viewStartMs;
}

/// A dedicated marker track for the take's notes: point pills / range bars at
/// their timeline positions (tap one to edit). Tap empty space to add a point
/// at that spot; drag across the lane to add a note spanning that range.
class NotesLane extends StatelessWidget {
  const NotesLane({
    required this.track,
    required this.timelineDurationMs,
    required this.position,
    required this.onAddNote,
    required this.onEditNote,
    this.viewStartMs = 0,
    super.key,
  });

  final AudioTrack track;
  final int timelineDurationMs;
  final int viewStartMs;
  final ValueListenable<Duration> position;

  /// Add a note: [startMs] only (point) when [endMs] is null, else the range.
  final void Function(int startMs, int? endMs) onAddNote;
  final ValueChanged<PracticeNote> onEditNote;

  @override
  Widget build(BuildContext context) {
    return _MarkerLane(
      label: 'NOTES',
      style: _MarkerStyle.note,
      timelineDurationMs: timelineDurationMs,
      viewStartMs: viewStartMs,
      position: position,
      onAdd: onAddNote,
      items: [
        for (final n in track.notes)
          _LaneItem(
            startMs: n.positionMs,
            endMs: n.endMs,
            label: n.content,
            onTap: () => onEditNote(n),
          ),
      ],
    );
  }
}

/// The technique track: a sub-lane mirroring [NotesLane], but its markers
/// reference techniques from the library. Tap empty space to add one (after
/// picking) at that spot; drag to add one spanning a range; tap a marker to
/// edit/remove it.
class TechniqueLane extends StatelessWidget {
  const TechniqueLane({
    required this.track,
    required this.timelineDurationMs,
    required this.position,
    required this.onAddTechnique,
    required this.onEditTechnique,
    this.viewStartMs = 0,
    super.key,
  });

  final AudioTrack track;
  final int timelineDurationMs;
  final int viewStartMs;
  final ValueListenable<Duration> position;

  /// Add a technique: [startMs] only (point) when [endMs] is null, else range.
  final void Function(int startMs, int? endMs) onAddTechnique;
  final ValueChanged<TechniqueMarker> onEditTechnique;

  @override
  Widget build(BuildContext context) {
    return _MarkerLane(
      label: 'TECHNIQUES',
      style: _MarkerStyle.technique,
      timelineDurationMs: timelineDurationMs,
      viewStartMs: viewStartMs,
      position: position,
      onAdd: onAddTechnique,
      items: [
        for (final m in track.techniqueMarkers)
          _LaneItem(
            startMs: m.positionMs,
            endMs: m.endMs,
            label: m.techniqueName,
            onTap: () => onEditTechnique(m),
          ),
      ],
    );
  }
}

/// One marker to render on a [_MarkerLane] — a point (when [endMs] is null) or
/// a range. Decouples the lane from the concrete note/technique model.
class _LaneItem {
  const _LaneItem({
    required this.startMs,
    required this.label,
    required this.onTap,
    this.endMs,
  });

  final int startMs;
  final int? endMs;
  final String label;
  final VoidCallback onTap;

  bool get isRange => endMs != null && endMs! > startMs;
}

/// Visual styling for a marker lane (notes are neutral; techniques are coral).
class _MarkerStyle {
  const _MarkerStyle({
    required this.icon,
    required this.iconColor,
    required this.fill,
    required this.border,
    required this.labelColor,
    required this.labelWeight,
  });

  final IconData icon;
  final Color iconColor;
  final Color fill;
  final Color border;
  final Color labelColor;
  final FontWeight labelWeight;

  static const note = _MarkerStyle(
    icon: FIcons.stickyNote,
    iconColor: Studio.accent,
    fill: Studio.surfaceRaised,
    border: Studio.border,
    labelColor: Studio.textSecondary,
    labelWeight: FontWeight.w400,
  );

  static final technique = _MarkerStyle(
    icon: FIcons.waypoints,
    iconColor: Studio.accent,
    fill: Studio.accent.withValues(alpha: 0.14),
    border: Studio.accent.withValues(alpha: 0.4),
    labelColor: Studio.accentBright,
    labelWeight: FontWeight.w600,
  );
}

/// Shared marker lane: renders [items] as point pills / range bars over the
/// timeline, with a playhead overlay. Tapping the empty background adds a point
/// at the tapped spot; dragging horizontally adds a marker spanning the drag
/// (with a live selection highlight). [onAdd] receives the start and, for a
/// drag, the end (null for a tap).
class _MarkerLane extends StatefulWidget {
  const _MarkerLane({
    required this.label,
    required this.style,
    required this.items,
    required this.timelineDurationMs,
    required this.viewStartMs,
    required this.position,
    required this.onAdd,
  });

  final String label;
  final _MarkerStyle style;
  final List<_LaneItem> items;
  final int timelineDurationMs;
  final int viewStartMs;
  final ValueListenable<Duration> position;
  final void Function(int startMs, int? endMs) onAdd;

  @override
  State<_MarkerLane> createState() => _MarkerLaneState();
}

class _MarkerLaneState extends State<_MarkerLane> {
  // While a range drag is in progress, the TIMES (ms) under its start and its
  // current pointer — taken at the view start of the moment each was touched.
  // Not pixels converted on release: follow-cursor can page the view while
  // the drag is held, and the release-time view start would then commit a
  // range the user never dragged over (and draw the highlight there too).
  int? _dragStartMs;
  int? _dragCurMs;

  void _resetDrag() => setState(() {
    _dragStartMs = null;
    _dragCurMs = null;
  });

  /// The held drag's range in time order, or null when none is held. A
  /// right-to-left drag must reach onAdd (and the editors, whose header tells
  /// a range from a point by end > start) as the same range a left-to-right
  /// one would.
  (int, int)? get _dragRange {
    final s = _dragStartMs, e = _dragCurMs;
    if (s == null || e == null) return null;
    return s < e ? (s, e) : (e, s);
  }

  @override
  Widget build(BuildContext context) {
    final dur = widget.timelineDurationMs;
    final body = LayoutBuilder(
      builder: (context, c) {
        final w = c.maxWidth;
        double xOf(int ms) =>
            dur <= 0 ? 0.0 : (ms - widget.viewStartMs) / dur * w;
        int msOf(double x) =>
            dur <= 0 ? 0 : (widget.viewStartMs + x / w * dur).round();

        void endDrag() {
          final range = _dragRange;
          if (range != null) widget.onAdd(range.$1, range.$2);
          _resetDrag();
        }

        final held = _dragRange;

        return Stack(
          children: [
            Positioned.fill(
              child: GestureDetector(
                behavior: HitTestBehavior.opaque,
                onTapUp: (d) =>
                    widget.onAdd(msOf(d.localPosition.dx.clamp(0.0, w)), null),
                onHorizontalDragStart: (d) => setState(() {
                  _dragStartMs = msOf(d.localPosition.dx.clamp(0.0, w));
                  _dragCurMs = _dragStartMs;
                }),
                onHorizontalDragUpdate: (d) => setState(
                  () => _dragCurMs = msOf(d.localPosition.dx.clamp(0.0, w)),
                ),
                onHorizontalDragEnd: (_) => endDrag(),
                onHorizontalDragCancel: _resetDrag,
                child: const ColoredBox(color: Studio.inset),
              ),
            ),
            // Live range-selection highlight while dragging, drawn from the
            // times so a page flip carries it with the content (clamped to
            // the box once part of it scrolls out).
            if (held != null)
              Positioned(
                left: xOf(held.$1).clamp(0.0, w),
                width: (xOf(held.$2).clamp(0.0, w) - xOf(held.$1).clamp(0.0, w))
                    .clamp(1.0, w),
                top: 0,
                bottom: 0,
                child: IgnorePointer(
                  child: DecoratedBox(
                    decoration: BoxDecoration(
                      color: widget.style.iconColor.withValues(alpha: 0.16),
                      border: Border.all(
                        color: widget.style.iconColor.withValues(alpha: 0.5),
                      ),
                    ),
                  ),
                ),
              ),
            for (final it in widget.items) ..._marker(it, xOf, w),
            Positioned.fill(
              child: PlayheadLayer(
                position: widget.position,
                timelineDurationMs: dur,
                startMs: widget.viewStartMs,
                color: Studio.playhead,
              ),
            ),
          ],
        );
      },
    );
    return _laneShell(widget.label, 56, body);
  }

  List<Widget> _marker(_LaneItem it, double Function(int) xOf, double w) {
    if (it.isRange) {
      final xs = xOf(it.startMs);
      final xe = xOf(it.endMs!);
      if (xe < -20 || xs > w + 20) return const [];
      // Clamp the left edge into the viewport so the icon + label (anchored at
      // the bar's left) stay visible when the range begins off-screen.
      final left = xs.clamp(0.0, w);
      return [
        Positioned(
          left: left,
          width: (xe - left).clamp(22.0, double.infinity),
          top: 6,
          bottom: 6,
          child: _MarkerChip(
            style: widget.style,
            label: it.label,
            onTap: it.onTap,
            fill: true,
          ),
        ),
      ];
    }
    final x = xOf(it.startMs);
    if (x < -20 || x > w + 20) return const [];
    return [
      Positioned(
        left: x,
        top: 6,
        bottom: 6,
        child: _MarkerChip(
          style: widget.style,
          label: it.label,
          onTap: it.onTap,
        ),
      ),
    ];
  }
}

/// A marker on the lane: a content-sized point pill, or (when [fill]) a bar that
/// stretches to fill its positioned width to show a range span.
class _MarkerChip extends StatelessWidget {
  const _MarkerChip({
    required this.style,
    required this.label,
    required this.onTap,
    this.fill = false,
  });

  final _MarkerStyle style;
  final String label;
  final VoidCallback onTap;
  final bool fill;

  @override
  Widget build(BuildContext context) {
    final text = Text(
      label,
      maxLines: 1,
      overflow: TextOverflow.ellipsis,
      style: TextStyle(
        color: style.labelColor,
        fontSize: 11,
        fontWeight: style.labelWeight,
      ),
    );
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onTap,
        child: Container(
          padding: EdgeInsets.symmetric(horizontal: fill ? 7 : 8, vertical: 4),
          alignment: fill ? Alignment.centerLeft : null,
          decoration: BoxDecoration(
            color: style.fill,
            borderRadius: BorderRadius.circular(4),
            border: Border.all(color: style.border),
          ),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(style.icon, size: 11, color: style.iconColor),
              const SizedBox(width: 5),
              // A point pill is bounded so a long label ellipsizes; a range bar
              // flexes to whatever width the span gives it.
              if (fill)
                Flexible(child: text)
              else
                ConstrainedBox(
                  constraints: const BoxConstraints(maxWidth: 150),
                  child: text,
                ),
            ],
          ),
        ),
      ),
    );
  }
}

/// Shared shell: an indented header column ([label]) + the lane [body], sized
/// and bordered like the other lanes.
Widget _laneShell(String label, double height, Widget body) {
  return Container(
    height: height,
    decoration: const BoxDecoration(
      border: Border(bottom: BorderSide(color: Studio.border)),
    ),
    child: Row(
      children: [
        Container(
          width: kTrackHeaderW,
          decoration: const BoxDecoration(
            color: Studio.panel,
            border: Border(right: BorderSide(color: Studio.border)),
          ),
          child: Padding(
            padding: const EdgeInsets.fromLTRB(30, 8, 8, 8),
            child: Align(
              alignment: Alignment.topLeft,
              child: Text(
                label,
                style: const TextStyle(
                  color: Studio.textFaint,
                  fontSize: 10,
                  fontWeight: FontWeight.w700,
                  letterSpacing: 0.8,
                ),
              ),
            ),
          ),
        ),
        Expanded(child: body),
      ],
    ),
  );
}
