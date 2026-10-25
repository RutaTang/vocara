import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';

import '../../core/themes/studio.dart';
import '../../core/widgets/studio_controls.dart';
import '../../core/widgets/waveform_painter.dart';
import '../view_model/song_practice_detail_view_model.dart';
import 'track_row.dart';

const double rulerHeight = 28;

/// The interactive time ruler. It is split like a track row: an empty corner
/// over the track-header column, then the timeline ticks. Dragging the ruler
/// body moves the playhead (scrub); the loop region (defaulting to the full
/// timeline) is resized only by dragging its left/right edge handles, so
/// scrubbing isn't hijacked.
class RulerBar extends StatelessWidget {
  const RulerBar({
    required this.vm,
    required this.timelineDurationMs,
    required this.position,
    super.key,
  });

  final SongPracticeDetailViewModel vm;
  final int timelineDurationMs;
  final ValueListenable<Duration> position;

  @override
  Widget build(BuildContext context) {
    final dur = timelineDurationMs;

    return Container(
      height: rulerHeight,
      color: Studio.surface,
      // Inset to match the lanes + drop boxes below, so ticks stay aligned with
      // the waveforms.
      padding: const EdgeInsets.symmetric(horizontal: kTimelineHMargin),
      child: Row(
        children: [
          // Corner over the track-header column — keeps ticks aligned with
          // each lane's waveform.
          Container(
            width: kTrackHeaderW,
            decoration: const BoxDecoration(
              border: Border(right: BorderSide(color: Studio.border)),
            ),
          ),
          Expanded(child: _ticks(context, dur)),
        ],
      ),
    );
  }

  Widget _ticks(BuildContext context, int dur) {
    final viewStart = vm.playback.viewStartMs;
    return LayoutBuilder(
      builder: (context, constraints) {
        final w = constraints.maxWidth;

        final ticks = CustomPaint(
          painter: TimeRulerPainter(
            timelineDurationMs: dur,
            startMs: viewStart,
            tickColor: Studio.gridStrong,
            textColor: Studio.textSecondary,
          ),
        );
        final playhead = PlayheadLayer(
          position: position,
          timelineDurationMs: dur,
          startMs: viewStart,
          color: Studio.playhead,
          showHead: true,
        );

        if (dur <= 0 || w <= 0) {
          return Stack(children: [Positioned.fill(child: ticks)]);
        }

        double xOf(int ms) => (ms - viewStart) / dur * w;
        int msAt(double dx) =>
            (viewStart + (dx / w).clamp(0.0, 1.0) * dur).round();
        final startX = xOf(vm.playback.loopStartMs);
        final endX = xOf(vm.playback.loopEndMs);
        // The fill is clamped to the viewport so a loop overflowing the left edge
        // still tints the whole visible region (and never overshoots the right);
        // the edge handles below stay UNCLAMPED so they track the true loop edges.
        // w > 0 here (guarded above), so clamp(0, w) can't assert (lower <= upper).
        final fillLeft = startX.clamp(0.0, w);
        final fillRight = endX.clamp(0.0, w);
        final loopColor = Studio.accent.withValues(
          alpha: vm.playback.loopEnabled ? 0.20 : 0.07,
        );

        return Stack(
          children: [
            // Body: tap or drag to move the playhead. Trackpad is excluded
            // so a two-finger horizontal swipe over the ruler does NOT scrub
            // (drag recognizers accept trackpad pan-zoom by default) — the
            // ruler is not a pan/scroll surface.
            Positioned.fill(
              child: GestureDetector(
                behavior: HitTestBehavior.opaque,
                supportedDevices: kPointerDragDevices,
                onTapUp: (d) =>
                    vm.seek(Duration(milliseconds: msAt(d.localPosition.dx))),
                onHorizontalDragUpdate: (d) =>
                    vm.seek(Duration(milliseconds: msAt(d.localPosition.dx))),
                child: const SizedBox.expand(),
              ),
            ),
            // Loop region fill.
            Positioned(
              left: fillLeft,
              width: fillRight - fillLeft,
              top: 0,
              bottom: 0,
              child: IgnorePointer(child: ColoredBox(color: loopColor)),
            ),
            Positioned.fill(child: IgnorePointer(child: ticks)),
            // Edge handles (only these resize the region).
            _LoopHandle(
              centerX: startX,
              onDelta: (dx) => vm.playback.setLoopStart(
                vm.playback.loopStartMs + (dx / w * dur).round(),
              ),
            ),
            _LoopHandle(
              centerX: endX,
              onDelta: (dx) => vm.playback.setLoopEnd(
                vm.playback.loopEndMs + (dx / w * dur).round(),
              ),
            ),
            Positioned.fill(child: playhead),
          ],
        );
      },
    );
  }
}

class _LoopHandle extends StatelessWidget {
  const _LoopHandle({required this.centerX, required this.onDelta});

  final double centerX;
  final ValueChanged<double> onDelta;

  @override
  Widget build(BuildContext context) {
    return Positioned(
      left: centerX - 8,
      width: 16,
      top: 0,
      bottom: 0,
      child: MouseRegion(
        cursor: SystemMouseCursors.resizeLeftRight,
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          // Not a trackpad swipe either: with the cursor on a handle, the
          // swipe meant to pan moved the loop edge.
          supportedDevices: kPointerDragDevices,
          onHorizontalDragUpdate: (d) => onDelta(d.primaryDelta ?? 0),
          child: Center(
            child: Container(
              width: 3,
              height: double.infinity,
              decoration: BoxDecoration(
                color: Studio.accent,
                borderRadius: BorderRadius.circular(2),
              ),
            ),
          ),
        ),
      ),
    );
  }
}
