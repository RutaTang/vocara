import 'package:flutter/material.dart';

import '../themes/studio.dart';

/// A tiny uppercase section label used across panels and stat blocks.
class CapLabel extends StatelessWidget {
  const CapLabel(this.text, {super.key});

  final String text;

  @override
  Widget build(BuildContext context) => Text(
    text.toUpperCase(),
    style: const TextStyle(
      color: Studio.textFaint,
      fontSize: 9,
      fontWeight: FontWeight.w700,
      letterSpacing: 0.5,
    ),
  );
}
