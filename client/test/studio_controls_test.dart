import 'package:flutter/gestures.dart'
    show PointerDeviceKind, kDoubleTapTimeout;
import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart' show RenderParagraph;
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/ui/core/widgets/studio_controls.dart';

void main() {
  group('StudioPanKnob', () {
    /// A knob wired the way every strip wires it: onChanged feeds the new
    /// value straight back in, which only lands on the next rebuild.
    Future<ValueNotifier<double>> pumpKnob(WidgetTester tester) async {
      final pan = ValueNotifier(0.0);
      addTearDown(pan.dispose);
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: Center(
              child: ValueListenableBuilder<double>(
                valueListenable: pan,
                builder: (_, v, _) => StudioPanKnob(
                  value: v,
                  onChanged: (next) => pan.value = next,
                  onCommit: () {},
                ),
              ),
            ),
          ),
        ),
      );
      return pan;
    }

    /// Presses on the knob and moves far enough for the pan to be accepted
    /// (that first move only wins the gesture), then lets a frame build.
    Future<TestGesture> grab(WidgetTester tester, double dx) async {
      final g = await tester.startGesture(
        tester.getCenter(find.byType(StudioPanKnob)),
        kind: PointerDeviceKind.mouse,
      );
      await g.moveBy(Offset(dx, 0));
      await tester.pump();
      return g;
    }

    // Regression guard: each move panned from the widget's value, which only
    // updates on rebuild, so two pointer moves inside one frame (a fast
    // mouse) both started from the same stale value and the first was lost.
    testWidgets('two moves inside one frame both count', (tester) async {
      final pan = await pumpKnob(tester);
      final g = await grab(tester, 20);
      final before = pan.value;

      await g.moveBy(const Offset(8, 0));
      await g.moveBy(const Offset(8, 0));
      await tester.pump();
      await g.up();

      expect(pan.value, closeTo(before + 16 / 80, 1e-9));
      // Let the knob's double-tap (re-center) recognizer time out.
      await tester.pump(kDoubleTapTimeout);
    });

    testWidgets('each drag starts from the value as it is now', (tester) async {
      final pan = await pumpKnob(tester);
      var g = await grab(tester, 20);
      await g.moveBy(const Offset(16, 0));
      await tester.pump();
      await g.up();
      await tester.pump(kDoubleTapTimeout);
      expect(pan.value, closeTo(16 / 80, 1e-9));

      // Changed elsewhere between drags (the inspector's knob for the same
      // channel, a sync): the next drag must not resume the last one's pan.
      pan.value = -0.5;
      await tester.pump();
      g = await grab(tester, -20);
      await g.moveBy(const Offset(-8, 0));
      await tester.pump();
      await g.up();
      expect(pan.value, closeTo(-0.5 - 8 / 80, 1e-9));
      await tester.pump(kDoubleTapTimeout);
    });
  });

  group('StudioFader', () {
    var ends = 0;
    Future<List<double>> pumpFader(WidgetTester tester) async {
      ends = 0;
      final changes = <double>[];
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: Center(
              child: SizedBox(
                width: 200,
                child: StudioFader(
                  value: 0.5,
                  accent: Colors.red,
                  onChanged: changes.add,
                  onChangeEnd: () => ends++,
                ),
              ),
            ),
          ),
        ),
      );
      return changes;
    }

    // A two-finger trackpad swipe pans the timeline; with the cursor over a
    // lane header's fader it reached the fader as a pan-zoom gesture and
    // jumped the take's volume.
    testWidgets('a two-finger trackpad swipe leaves the level alone', (
      tester,
    ) async {
      final changes = await pumpFader(tester);
      final at = tester.getCenter(find.byType(StudioFader));
      final g = await tester.createGesture(kind: PointerDeviceKind.trackpad);
      await g.panZoomStart(at);
      await tester.pump();
      await g.panZoomUpdate(at, pan: const Offset(-40, 0));
      await tester.pump();
      await g.panZoomUpdate(at, pan: const Offset(-80, 0));
      await tester.pump();
      await g.panZoomEnd();
      await tester.pump();
      expect(changes, isEmpty);
    });

    testWidgets('a mouse drag still sets the level', (tester) async {
      final changes = await pumpFader(tester);
      final at = tester.getCenter(find.byType(StudioFader));
      final g = await tester.startGesture(at, kind: PointerDeviceKind.mouse);
      await g.moveBy(const Offset(20, 0)); // wins the gesture
      await tester.pump();
      await g.moveBy(const Offset(20, 0));
      await tester.pump();
      await g.up();
      await tester.pump();
      expect(changes, isNotEmpty);
      expect(changes.last, greaterThan(0.5));
      expect(ends, 1);
    });

    testWidgets('a click jumps the level and commits it', (tester) async {
      final changes = await pumpFader(tester);
      final box = tester.getRect(find.byType(StudioFader));
      await tester.tapAt(
        Offset(box.left + box.width * 0.25, box.center.dy),
        kind: PointerDeviceKind.mouse,
      );
      await tester.pump();
      expect(changes.single, closeTo(0.25, 0.01));
      expect(ends, 1);
    });
  });

  group('FaderDbReadout', () {
    // In its 40 px box every level below -10 dB wrapped onto two lines
    // ("-14.0" over "dB") in a font as wide as the system one; the test font,
    // a square em per glyph, wrapped it onto three and overflowed the lane
    // headers it sits in.
    testWidgets('reads on one line, inside its width, at any level', (
      tester,
    ) async {
      for (final (volume, text) in [
        (1.0, '0.0 dB'),
        (0.2, '-14.0 dB'),
        (0.00011, '-79.2 dB'),
        (0.0, '-∞ dB'),
      ]) {
        await tester.pumpWidget(
          Directionality(
            textDirection: TextDirection.ltr,
            child: Center(child: FaderDbReadout(volume)),
          ),
        );
        final line = find.text(text);
        expect(line, findsOneWidget);
        final paragraph = tester.renderObject<RenderParagraph>(line);
        final oneLine = TextPainter(
          text: paragraph.text,
          textDirection: TextDirection.ltr,
          maxLines: 1,
        )..layout();
        addTearDown(oneLine.dispose);
        expect(paragraph.size.height, oneLine.height, reason: text);
        final box = tester.getRect(find.byType(FaderDbReadout));
        final drawn = tester.getRect(line);
        expect(box.width, 46);
        expect(drawn.left, greaterThanOrEqualTo(box.left - 0.01), reason: text);
        expect(drawn.right, closeTo(box.right, 0.01), reason: text);
      }
    });
  });

  group('FixedWidthReadout', () {
    // The FX panel's 42 px value box: "1444 ms" (a delay time) set 42.3 px
    // wide in a font as wide as the system one and wrapped onto two lines.
    testWidgets('a reading wider than its box scales down onto one line', (
      tester,
    ) async {
      await tester.pumpWidget(
        const Directionality(
          textDirection: TextDirection.ltr,
          child: Center(
            child: FixedWidthReadout(
              '1444 ms',
              width: 42,
              style: TextStyle(fontSize: 10.5),
            ),
          ),
        ),
      );
      final paragraph = tester.renderObject<RenderParagraph>(
        find.text('1444 ms'),
      );
      final oneLine = TextPainter(
        text: paragraph.text,
        textDirection: TextDirection.ltr,
        maxLines: 1,
      )..layout();
      addTearDown(oneLine.dispose);
      expect(paragraph.size.height, oneLine.height);
      final box = tester.getRect(find.byType(FixedWidthReadout));
      final drawn = tester.getRect(find.text('1444 ms'));
      expect(box.width, 42);
      expect(drawn.left, greaterThanOrEqualTo(box.left - 0.01));
      expect(drawn.right, closeTo(box.right, 0.01));
    });
  });
}
