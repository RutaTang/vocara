import 'dart:async';

import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:flutter/rendering.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/ui/core/widgets/stable_subtree.dart';
import 'package:vocara/ui/song_practice_detail/view_model/mixer_controller.dart';

/// Checks a screen that keeps unchanged subtrees across rebuilds
/// ([StableSubtree], the mixer's envelope cache) against a fresh build of the
/// same state: both memoizations switched off by their test flags, compared
/// pixel for pixel and text for text. A keep is only sound if its comparator
/// sees every input the subtree shows; one that misses an input shows up as
/// a step whose kept screen differs from the fresh one.
class FreshBuildOracle {
  FreshBuildOracle(this.tester) {
    addTearDown(() {
      StableSubtree.debugNeverKeep = false;
      MixerController.debugNoEnvelopeCache = false;
    });
  }

  final WidgetTester tester;

  /// The screen the shots are taken of: [wrap] the app in it.
  final _root = GlobalKey();

  /// The steps whose kept screen differed from the fresh build.
  final failures = <String>[];

  Widget wrap(Widget app) => RepaintBoundary(key: _root, child: app);

  /// Lets the screen settle, then records [what] in [failures] if the kept
  /// screen differs from a fresh build.
  Future<void> expectFresh(String what) async {
    for (var i = 0; i < 20; i++) {
      await tester.pump(const Duration(milliseconds: 100));
    }
    // Two kept shots that agree: a screenshot runs real async, in which work
    // still in flight (a lane's analysis) can land and change the screen.
    late Uint8List kept;
    late List<String> keptTexts;
    var noise = 0;
    for (var attempt = 0; attempt < 5; attempt++) {
      await _rebuild();
      final first = await _shot();
      final firstTexts = _texts();
      await _rebuild();
      kept = await _shot();
      keptTexts = _texts();
      noise = _diff(first, kept);
      if (noise == 0 && firstTexts.join('|') == keptTexts.join('|')) break;
    }
    StableSubtree.debugNeverKeep = true;
    MixerController.debugNoEnvelopeCache = true;
    Uint8List fresh;
    List<String> freshTexts;
    try {
      await _rebuild();
      fresh = await _shot();
      freshTexts = _texts();
    } finally {
      StableSubtree.debugNeverKeep = false;
      MixerController.debugNoEnvelopeCache = false;
    }
    await _rebuild();
    final d = _diff(kept, fresh);
    if (d != 0 || keptTexts.join('|') != freshTexts.join('|')) {
      final a = keptTexts.toSet(), b = freshTexts.toSet();
      failures.add(
        '$what: $d pixels differ (run-to-run noise $noise); '
        'kept-only texts ${a.difference(b)}, fresh-only ${b.difference(a)}',
      );
    }
  }

  /// Makes [change], lets a frame or two pass, and checks the result — an
  /// asynchronous change is not awaited, as a user doesn't wait for one.
  Future<void> step(String what, FutureOr<void> Function() change) async {
    final r = change();
    if (r is Future) {
      unawaited(r.catchError((Object e) => failures.add('$what threw: $e')));
    }
    await tester.pump(const Duration(milliseconds: 50));
    await tester.pump(const Duration(milliseconds: 50));
    await expectFresh(what);
  }

  /// Rebuilds every element, so each kept subtree is offered its new child.
  Future<void> _rebuild() async {
    void mark(Element e) {
      e.markNeedsBuild();
      e.visitChildren(mark);
    }

    tester.binding.rootElement!.visitChildren(mark);
    await tester.pump(const Duration(milliseconds: 16));
  }

  Future<Uint8List> _shot() async {
    final boundary =
        _root.currentContext!.findRenderObject()! as RenderRepaintBoundary;
    final bytes = await tester.runAsync(() async {
      final image = await boundary.toImage();
      final data = await image.toByteData(format: ui.ImageByteFormat.rawRgba);
      image.dispose();
      return data!.buffer.asUint8List();
    });
    return bytes!;
  }

  List<String> _texts() => [
    for (final p in tester.allRenderObjects.whereType<RenderParagraph>())
      p.text.toPlainText(),
  ];

  /// Pixels whose colour differs (alpha ignored); -1 if the sizes differ.
  static int _diff(Uint8List a, Uint8List b) {
    if (a.length != b.length) return -1;
    var n = 0;
    for (var i = 0; i < a.length; i += 4) {
      if (a[i] != b[i] || a[i + 1] != b[i + 1] || a[i + 2] != b[i + 2]) n++;
    }
    return n;
  }
}
