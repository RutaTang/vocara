import 'package:desktop_drop/desktop_drop.dart';
import 'package:flutter/gestures.dart' show HitTestResult;
import 'package:flutter/widgets.dart';

/// A [DropTarget] that takes a file drop only where a click would reach it.
///
/// desktop_drop decides by the target's rectangle alone, never by what is
/// drawn over it, so a drop landed on whatever lay beneath the pointer: a
/// lane under a dialog's backdrop or the lyrics focus overlay, or the part
/// of a lane scrolled out of its list and hidden behind the dock or the
/// transport bar — imported, silently, into a take the user could not see.
class VisibleDropTarget extends StatefulWidget {
  const VisibleDropTarget({
    required this.onDrop,
    required this.child,
    this.onHover,
    this.enable = true,
    super.key,
  });

  /// The dropped files' paths, for a drop this target can be seen under.
  final void Function(List<String> paths) onDrop;

  /// Whether a drag over it is one it would take, as that changes.
  final ValueChanged<bool>? onHover;

  final bool enable;
  final Widget child;

  @override
  State<VisibleDropTarget> createState() => _VisibleDropTargetState();
}

class _VisibleDropTargetState extends State<VisibleDropTarget> {
  /// What a hit test at the drop must reach: in the hit-test path only
  /// where nothing above it takes the point and it is not clipped away.
  final GlobalKey _probe = GlobalKey();

  bool _hover = false;

  bool _reaches(Offset global) {
    final target = _probe.currentContext?.findRenderObject();
    if (target is! RenderBox || !target.attached) return false;
    final result = HitTestResult();
    WidgetsBinding.instance.hitTestInView(
      result,
      global,
      View.of(context).viewId,
    );
    return result.path.any((entry) => identical(entry.target, target));
  }

  void _setHover(bool hover) {
    if (hover == _hover || !mounted) return;
    setState(() => _hover = hover);
    widget.onHover?.call(hover);
  }

  @override
  Widget build(BuildContext context) {
    return DropTarget(
      enable: widget.enable,
      onDragEntered: (d) => _setHover(_reaches(d.globalPosition)),
      onDragUpdated: (d) => _setHover(_reaches(d.globalPosition)),
      onDragExited: (_) => _setHover(false),
      onDragDone: (d) {
        final reached = _reaches(d.globalPosition);
        _setHover(false);
        if (reached) widget.onDrop([for (final f in d.files) f.path]);
      },
      // Translucent: in the path wherever it lies on top, and no change to
      // what anything beneath it or inside it receives.
      child: Listener(
        key: _probe,
        behavior: HitTestBehavior.translucent,
        child: widget.child,
      ),
    );
  }
}
