import 'package:flutter/material.dart';
import 'package:window_manager/window_manager.dart';

import '../../../config/environment.dart';
import '../themes/studio.dart';

/// A slim top bar acting as the frameless window's drag region (like Electron's
/// `-webkit-app-region: drag`). The macOS traffic lights float over its left;
/// dragging moves the window and a double-click toggles maximize — both handled
/// by [DragToMoveArea].
class WindowDragBar extends StatelessWidget {
  const WindowDragBar({super.key});

  static const double height = 28;

  @override
  Widget build(BuildContext context) {
    final badge = AppEnvironment.current.badge;
    return DragToMoveArea(
      child: Container(
        height: height,
        width: double.infinity,
        color: Studio.surface,
        // Coral environment pill on the right (DEV / TEST); nothing in prod.
        // Right-aligned to clear the traffic lights floating over the left.
        alignment: Alignment.centerRight,
        padding: const EdgeInsets.only(right: 10),
        child: badge.isEmpty ? null : _EnvBadge(label: badge),
      ),
    );
  }
}

/// Small uppercase pill marking the non-production environment.
class _EnvBadge extends StatelessWidget {
  const _EnvBadge({required this.label});

  final String label;

  @override
  Widget build(BuildContext context) {
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
      decoration: BoxDecoration(
        color: Studio.accent,
        borderRadius: BorderRadius.circular(4),
      ),
      child: Text(
        label,
        style: const TextStyle(
          color: Studio.background,
          fontSize: 9,
          fontWeight: FontWeight.w800,
          letterSpacing: 0.8,
          height: 1.0,
        ),
      ),
    );
  }
}
