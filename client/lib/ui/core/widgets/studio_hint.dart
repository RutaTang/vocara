import 'package:flutter/material.dart';

import '../themes/studio.dart';

/// A centered faint hint for a dock view's empty / loading state, with an
/// optional spinner while an analysis is running.
class StudioHint extends StatelessWidget {
  const StudioHint(
    this.text, {
    this.spinner = false,
    this.fontSize = 13,
    this.padding = EdgeInsets.zero,
    super.key,
  });

  final String text;
  final bool spinner;
  final double fontSize;
  final EdgeInsetsGeometry padding;

  @override
  Widget build(BuildContext context) {
    return Center(
      child: Padding(
        padding: padding,
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            if (spinner) ...[
              const SizedBox(
                width: 18,
                height: 18,
                child: CircularProgressIndicator(
                  strokeWidth: 2,
                  valueColor: AlwaysStoppedAnimation(Studio.accent),
                ),
              ),
              const SizedBox(height: 14),
            ],
            Text(
              text,
              textAlign: TextAlign.center,
              style: TextStyle(
                color: Studio.textFaint,
                fontSize: fontSize,
                height: 1.4,
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// The inset empty / loading state that fills a full-width analysis lane.
class LaneHint extends StatelessWidget {
  const LaneHint(this.text, {super.key});

  final String text;

  @override
  Widget build(BuildContext context) {
    return ColoredBox(
      color: Studio.inset,
      child: Center(
        child: Text(
          text,
          style: const TextStyle(color: Studio.textFaint, fontSize: 12),
        ),
      ),
    );
  }
}
