import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';

import '../../../data/services/monitor_service.dart';
import '../../core/themes/studio.dart';
import '../../core/widgets/spectrogram_painter.dart';

/// Scrolling live spectrogram driven by [MonitorService]. Rebuilds its image
/// at most once per frame as new mic columns arrive (oldest on the left).
class LiveSpectrogram extends StatefulWidget {
  const LiveSpectrogram({required this.monitor, super.key});

  final MonitorService monitor;

  @override
  State<LiveSpectrogram> createState() => _LiveSpectrogramState();
}

class _LiveSpectrogramState extends State<LiveSpectrogram> {
  ui.Image? _image;
  bool _scheduled = false;
  int _generation = 0;

  @override
  void initState() {
    super.initState();
    widget.monitor.addListener(_onChange);
    _rebuild();
  }

  @override
  void didUpdateWidget(LiveSpectrogram old) {
    super.didUpdateWidget(old);
    if (!identical(old.monitor, widget.monitor)) {
      old.monitor.removeListener(_onChange);
      widget.monitor.addListener(_onChange);
    }
  }

  @override
  void dispose() {
    widget.monitor.removeListener(_onChange);
    _image?.dispose();
    super.dispose();
  }

  void _onChange() {
    if (_scheduled) return;
    _scheduled = true;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      _scheduled = false;
      if (mounted) _rebuild();
    });
  }

  Future<void> _rebuild() async {
    final cols = widget.monitor.columns;
    final bins = widget.monitor.bins;
    if (cols.isEmpty) {
      // Invalidate any in-flight render so it can't install a stale image after we
      // clear (the monitor was reset to no columns).
      ++_generation;
      if (_image != null && mounted) {
        final old = _image;
        setState(() => _image = null);
        old?.dispose();
      }
      return;
    }
    // Snapshot both lists synchronously (same length, evicted in tandem) so the
    // voicing mask stays aligned with the columns across the await below.
    final colsCopy = List<Float32List>.of(cols);
    final activeCopy = List<int>.of(widget.monitor.activeColumns);
    final gen = ++_generation;
    final image = await renderColumnsImage(colsCopy, bins, active: activeCopy);
    // Drop a stale render: unmounted, or a newer _rebuild started while this one
    // awaited (renders aren't guaranteed to complete in submission order), so an
    // earlier frame completing last would install a stale image.
    if (!mounted || gen != _generation) {
      image.dispose();
      return;
    }
    final old = _image;
    setState(() => _image = image);
    old?.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final image = _image;
    return Stack(
      children: [
        const Positioned.fill(child: ColoredBox(color: Studio.inset)),
        if (image != null)
          Positioned.fill(child: CustomPaint(painter: _LivePainter(image))),
        Positioned.fill(
          child: IgnorePointer(
            child: CustomPaint(
              painter: FrequencyGridPainter(maxHz: widget.monitor.maxHz),
            ),
          ),
        ),
        if (image == null)
          const Center(
            child: Text(
              'Sing to see your voiceprint…',
              style: TextStyle(color: Studio.textFaint, fontSize: 13),
            ),
          ),
      ],
    );
  }
}

class _LivePainter extends CustomPainter {
  _LivePainter(this.image);

  final ui.Image image;

  @override
  void paint(Canvas canvas, Size size) {
    final src = Rect.fromLTWH(
      0,
      0,
      image.width.toDouble(),
      image.height.toDouble(),
    );
    final dst = Rect.fromLTWH(0, 0, size.width, size.height);
    canvas.drawImageRect(
      image,
      src,
      dst,
      Paint()..filterQuality = FilterQuality.medium,
    );
  }

  @override
  bool shouldRepaint(_LivePainter old) => old.image != image;
}
