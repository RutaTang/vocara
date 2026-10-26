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

        // A layer of its own: the ruler rebuilds on every notification of
        // the view model (a fader or clip-drag tick included), and the
        // relayout that follows repainted the ticks — laying out every time
        // label again (~0.4 ms in a debug build) when the view hadn't moved.
        final ticks = RepaintBoundary(
          child: CustomPaint(
            painter: TimeRulerPainter(
              timelineDurationMs: dur,
              startMs: viewStart,
              tickColor: Studio.gridStrong,
              textColor: Studio.textSecondary,
            ),
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
            // Edge handles (only these resize the region). A click on one
            // still seeks, as on the rest of the ruler: the default loop
            // starts at 0, and its handle covered the click back to 0:00.
            _LoopHandle(
              centerX: startX,
              valueMs: vm.playback.loopStartMs,
              msPerPx: dur / w,
              onChanged: vm.playback.setLoopStart,
              onTapAt: (dx) => vm.seek(Duration(milliseconds: msAt(dx))),
            ),
            _LoopHandle(
              centerX: endX,
              valueMs: vm.playback.loopEndMs,
              msPerPx: dur / w,
              onChanged: vm.playback.setLoopEnd,
              onTapAt: (dx) => vm.seek(Duration(milliseconds: msAt(dx))),
              // Drawn over the start's grip when the region is narrower than
              // half a handle: a drag leftward there moves the start, or the
              // region could only be widened to the right. Wider, the start's
              // grip is still there to take, and a leftward drag on the
              // end's own grip shrinks the region.
              leftward: (endX - startX).abs() < _LoopHandle.width / 2
                  ? (
                      valueMs: vm.playback.loopStartMs,
                      onChanged: vm.playback.setLoopStart,
                    )
                  : null,
            ),
            Positioned.fill(child: playhead),
          ],
        );
      },
    );
  }
}

class _LoopHandle extends StatefulWidget {
  const _LoopHandle({
    required this.centerX,
    required this.valueMs,
    required this.msPerPx,
    required this.onChanged,
    required this.onTapAt,
    this.leftward,
  });

  /// How wide a handle is to grab.
  static const double width = 16;

  final double centerX;

  /// The loop edge this handle drags.
  final int valueMs;
  final double msPerPx;
  final ValueChanged<int> onChanged;

  /// A click on the handle, at this x on the ruler.
  final ValueChanged<double> onTapAt;

  /// The edge a drag that starts leftward moves instead, when one is given.
  final ({int valueMs, ValueChanged<int> onChanged})? leftward;

  @override
  State<_LoopHandle> createState() => _LoopHandleState();
}

class _LoopHandleState extends State<_LoopHandle> {
  // The edge where this drag began and how far the pointer has moved since:
  // each tick sets the edge from the whole distance. Adding each tick's own
  // rounded delta lost the fraction every time, so at full zoom (about 1.3 ms
  // per px) a slow drag left the handle trailing the pointer, or not moving.
  int _baseMs = 0;
  double _dx = 0;

  /// The edge this drag moves, chosen by its first movement.
  ValueChanged<int>? _edge;

  @override
  Widget build(BuildContext context) {
    final left = widget.centerX - _LoopHandle.width / 2;
    return Positioned(
      left: left,
      width: _LoopHandle.width,
      top: 0,
      bottom: 0,
      child: MouseRegion(
        cursor: SystemMouseCursors.resizeLeftRight,
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          // Not a trackpad swipe either: with the cursor on a handle, the
          // swipe meant to pan moved the loop edge.
          supportedDevices: kPointerDragDevices,
          onTapUp: (d) => widget.onTapAt(left + d.localPosition.dx),
          onHorizontalDragStart: (_) {
            _edge = null;
            _dx = 0;
          },
          onHorizontalDragUpdate: (d) {
            _dx += d.primaryDelta ?? 0;
            if (_edge == null) {
              if (_dx == 0) return;
              final other = widget.leftward;
              if (other != null && _dx < 0) {
                _edge = other.onChanged;
                _baseMs = other.valueMs;
              } else {
                _edge = widget.onChanged;
                _baseMs = widget.valueMs;
              }
            }
            _edge!(_baseMs + (_dx * widget.msPerPx).round());
          },
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
