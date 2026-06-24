import 'package:flutter/material.dart';
import 'package:window_manager/window_manager.dart';

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
    return DragToMoveArea(
      child: Container(
        height: height,
        width: double.infinity,
        color: Studio.surface,
      ),
    );
  }
}
