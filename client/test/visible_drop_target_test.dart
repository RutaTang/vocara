// A file drop is decided by desktop_drop on the target's rectangle alone, so
// a target took drops through whatever was drawn over it: a dialog, the
// lyrics focus overlay, or the dock over a lane scrolled out of its list.

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/ui/core/widgets/visible_drop_target.dart';

/// Sends a drop of [paths] at [at] the way the platform plugin does.
Future<void> _drop(WidgetTester tester, Offset at, List<String> paths) async {
  const codec = StandardMethodCodec();
  Future<void> send(String method, Object? args) =>
      tester.binding.defaultBinaryMessenger.handlePlatformMessage(
        'desktop_drop',
        codec.encodeMethodCall(MethodCall(method, args)),
        (_) {},
      );
  await send('entered', [at.dx, at.dy]);
  await send('updated', [at.dx, at.dy]);
  await send('performOperation', paths);
  await tester.pump();
}

void main() {
  late List<List<String>> drops;
  late List<bool> hovers;

  Widget target() => VisibleDropTarget(
    onDrop: drops.add,
    onHover: hovers.add,
    child: const SizedBox(width: 300, height: 200),
  );

  setUp(() {
    drops = [];
    hovers = [];
  });

  testWidgets('takes a drop where it can be seen', (tester) async {
    await tester.pumpWidget(
      Directionality(
        textDirection: TextDirection.ltr,
        child: Align(alignment: Alignment.topLeft, child: target()),
      ),
    );
    await _drop(tester, const Offset(100, 100), ['/a.wav']);
    expect(drops, [
      ['/a.wav'],
    ]);
    expect(hovers, [true, false]);
  });

  testWidgets('takes no drop through something drawn over it', (tester) async {
    await tester.pumpWidget(
      Directionality(
        textDirection: TextDirection.ltr,
        child: Stack(
          children: [
            Align(alignment: Alignment.topLeft, child: target()),
            // A dialog's backdrop, or a full-window overlay.
            const Positioned.fill(child: ColoredBox(color: Color(0x80000000))),
          ],
        ),
      ),
    );
    await _drop(tester, const Offset(100, 100), ['/a.wav']);
    expect(drops, isEmpty);
    expect(hovers, isEmpty);
  });

  testWidgets('takes no drop where it is scrolled out of its list', (
    tester,
  ) async {
    final scroll = ScrollController();
    addTearDown(scroll.dispose);
    await tester.pumpWidget(
      Directionality(
        textDirection: TextDirection.ltr,
        child: Column(
          children: [
            // The list shows its first 100 px; the target lies past them,
            // laid out where the panel below it is drawn.
            SizedBox(
              height: 100,
              child: ListView(
                controller: scroll,
                children: [
                  const SizedBox(height: 120),
                  Align(alignment: Alignment.topLeft, child: target()),
                ],
              ),
            ),
            const Expanded(child: ColoredBox(color: Color(0xFF202020))),
          ],
        ),
      ),
    );
    await _drop(tester, const Offset(100, 150), ['/a.wav']);
    expect(drops, isEmpty, reason: 'the panel is what is there');

    scroll.jumpTo(120);
    await tester.pump();
    await _drop(tester, const Offset(100, 50), ['/b.wav']);
    expect(drops, [
      ['/b.wav'],
    ]);
  });
}
