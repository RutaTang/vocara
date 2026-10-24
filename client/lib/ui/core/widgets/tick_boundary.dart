import 'package:flutter/widgets.dart';

/// Keeps a subtree that rebuilds on a fast tick — a live take's waveform, an
/// input meter — from re-laying-out and repainting what surrounds it.
///
/// Rebuilding anything under a [LayoutBuilder] re-runs that LayoutBuilder's
/// layout, which marks every ancestor up to the nearest relayout boundary for
/// layout, and everything that lays out again repaints. Under the timeline's
/// LayoutBuilders that chain reached the Scaffold, so a 20 Hz recording tick
/// repainted the whole screen. This widget is a LayoutBuilder of its own (the
/// tick's layout work starts and ends here) under a [RepaintBoundary]. It can
/// only stop the relayout where its constraints are tight: mount it where the
/// parent fixes its size (a `Positioned.fill`, a sized box).
class TickBoundary extends StatelessWidget {
  const TickBoundary({required this.child, super.key});

  final Widget child;

  @override
  Widget build(BuildContext context) {
    return RepaintBoundary(
      child: LayoutBuilder(builder: (context, _) => child),
    );
  }
}
