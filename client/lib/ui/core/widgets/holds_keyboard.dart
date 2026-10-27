import 'package:flutter/widgets.dart';

/// Takes the keyboard while it is shown, for an overlay that covers controls
/// still in the tree beneath it, and hands it back to whatever had it when
/// the overlay goes.
///
/// Beneath the lyrics focus view the panel kept it: Space stamped lines no
/// one could see (wrapping round to overwrite good timings), and typing went
/// into the hidden editor, which its Save then kept. A scope of its own, so
/// Tab cannot walk back to them either.
class HoldsKeyboard extends StatefulWidget {
  const HoldsKeyboard({required this.child, super.key});

  final Widget child;

  @override
  State<HoldsKeyboard> createState() => _HoldsKeyboardState();
}

class _HoldsKeyboardState extends State<HoldsKeyboard> {
  final FocusScopeNode _node = FocusScopeNode(debugLabel: 'HoldsKeyboard');

  @override
  void initState() {
    super.initState();
    // Not autofocus: that only applies when nothing in the scope has focus,
    // and the panel beneath has it.
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) _node.requestFocus();
    });
  }

  @override
  void dispose() {
    // Detaching a focused node returns focus to the one before it.
    _node.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) =>
      FocusScope(node: _node, child: widget.child);
}
