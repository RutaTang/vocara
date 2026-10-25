import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/ui/core/widgets/stable_subtree.dart';

class _Leaf extends StatelessWidget {
  const _Leaf(this.value, this.label);

  final int value;
  final String label;

  @override
  Widget build(BuildContext context) =>
      Text('$label $value', textDirection: TextDirection.ltr);
}

/// [label] stands in for an input the comparator does not look at — like
/// code a hot reload swaps out under an unchanged value.
Widget _kept(int value, String label) => StableSubtree<_Leaf>(
  sameAs: (next, shown) => next.value == shown.value,
  child: _Leaf(value, label),
);

void main() {
  testWidgets('keeps the shown child while the comparator holds', (
    tester,
  ) async {
    await tester.pumpWidget(_kept(1, 'a'));
    await tester.pumpWidget(_kept(1, 'b'));
    expect(find.text('a 1'), findsOneWidget);

    await tester.pumpWidget(_kept(2, 'b'));
    expect(find.text('b 2'), findsOneWidget);
  });

  testWidgets('a hot reload refreshes a kept subtree', (tester) async {
    var label = 'old';
    late StateSetter rebuild;
    await tester.pumpWidget(
      StatefulBuilder(
        builder: (context, setState) {
          rebuild = setState;
          return _kept(1, label);
        },
      ),
    );
    expect(find.text('old 1'), findsOneWidget);

    label = 'new';
    final reassembled = tester.binding.reassembleApplication();
    await tester.pump();
    await reassembled;
    expect(find.text('new 1'), findsOneWidget);

    // Only that one rebuild: the keep resumes after it.
    label = 'newer';
    rebuild(() {});
    await tester.pump();
    expect(find.text('new 1'), findsOneWidget);
  });

  testWidgets('debugNeverKeep builds every child afresh', (tester) async {
    addTearDown(() => StableSubtree.debugNeverKeep = false);
    StableSubtree.debugNeverKeep = true;
    await tester.pumpWidget(_kept(1, 'a'));
    await tester.pumpWidget(_kept(1, 'b'));
    expect(find.text('b 1'), findsOneWidget);
  });
}
