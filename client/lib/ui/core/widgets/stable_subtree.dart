import 'package:flutter/widgets.dart';

/// Keeps showing the [child] instance it last adopted for as long as [sameAs]
/// says a new one shows the same thing, so Flutter skips that subtree's
/// rebuild — and the relayout and repaint a rebuild under a LayoutBuilder
/// drags along.
///
/// The song screen rebuilds from one view-model notification, and its live
/// edits (a fader, pan-knob or FX tick, a clip drag) notify at pointer rate
/// while changing one lane or one channel strip: every other lane and strip
/// was rebuilt, re-laid-out and repainted with it, for nothing.
///
/// [sameAs] decides what counts as unchanged. A widget's callbacks are
/// usually rebuilt every time and cannot be compared, so a [sameAs] that
/// ignores them must compare by identity every value they capture that can
/// change (an edited track, say, is a new object), or they act on stale state.
/// Inherited-widget dependencies still rebuild a kept subtree on their own.
class StableSubtree<T extends Widget> extends StatefulWidget {
  const StableSubtree({required this.child, required this.sameAs, super.key});

  /// Turns keeping off everywhere: every subtree shows the child it is
  /// handed. The oracle for tests that check a kept screen shows exactly
  /// what a freshly built one does — the only guard a comparator that misses
  /// a value has (counting rebuilds can't see what is shown).
  @visibleForTesting
  static bool debugNeverKeep = false;

  final T child;

  /// Whether [next] shows exactly what [shown], the child on screen, does.
  final bool Function(T next, T shown) sameAs;

  @override
  State<StableSubtree<T>> createState() => _StableSubtreeState<T>();
}

class _StableSubtreeState<T extends Widget> extends State<StableSubtree<T>> {
  late T _shown = widget.child;

  /// A hot reload changed code [StableSubtree.sameAs] doesn't look at (a
  /// callback, a label): the next child is adopted whatever it says.
  bool _reassembled = false;

  @override
  void reassemble() {
    super.reassemble();
    _reassembled = true;
  }

  @override
  void didUpdateWidget(StableSubtree<T> old) {
    super.didUpdateWidget(old);
    if (_reassembled ||
        StableSubtree.debugNeverKeep ||
        !widget.sameAs(widget.child, _shown)) {
      _shown = widget.child;
    }
    _reassembled = false;
  }

  @override
  Widget build(BuildContext context) {
    if (StableSubtree.debugNeverKeep) _shown = widget.child;
    return _shown;
  }
}
