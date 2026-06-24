import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:forui/forui.dart';
import 'package:go_router/go_router.dart';

import 'routing/router.dart';
import 'ui/core/themes/studio.dart';
import 'ui/core/themes/theme.dart';

/// App-wide scroll behavior: clamping physics everywhere (no rubber-band
/// overscroll bounce), on every list / scroll view in the app.
class _NoBounceScrollBehavior extends MaterialScrollBehavior {
  const _NoBounceScrollBehavior();

  @override
  ScrollPhysics getScrollPhysics(BuildContext context) =>
      const ClampingScrollPhysics();
}

/// Root widget. Wires forui's theme + localizations into a router-driven
/// [MaterialApp]. The Material layer provides Navigator/overlays that several
/// forui widgets (dialogs, text fields) build on; forui paints the surface.
class VocaraApp extends StatefulWidget {
  const VocaraApp({super.key});

  @override
  State<VocaraApp> createState() => _VocaraAppState();
}

class _VocaraAppState extends State<VocaraApp> {
  // Built once so the router config is stable across rebuilds.
  final GoRouter _router = createRouter();
  final GlobalKey _rootKey = GlobalKey();

  @override
  void initState() {
    super.initState();
    // Dev-only preview hook (no-op unless built with
    // `--dart-define=SCREENSHOT=/path.png`): writes a PNG of the rendered app
    // via the engine, then exits. Avoids needing OS screen-recording access.
    const shot = String.fromEnvironment('SCREENSHOT');
    if (shot.isNotEmpty) {
      WidgetsBinding.instance.addPostFrameCallback((_) => _capture(shot));
    }
  }

  Future<void> _capture(String path) async {
    await Future<void>.delayed(const Duration(seconds: 4));
    final boundary =
        _rootKey.currentContext?.findRenderObject() as RenderRepaintBoundary?;
    if (boundary != null) {
      final image = await boundary.toImage(pixelRatio: 1.5);
      final data = await image.toByteData(format: ui.ImageByteFormat.png);
      if (data != null) {
        File(path).writeAsBytesSync(data.buffer.asUint8List());
      }
    }
    exit(0);
  }

  @override
  Widget build(BuildContext context) {
    return MaterialApp.router(
      title: 'Vocara',
      debugShowCheckedModeBanner: false,
      scrollBehavior: const _NoBounceScrollBehavior(),
      theme: appTheme.toApproximateMaterialTheme().copyWith(
        scaffoldBackgroundColor: Studio.background,
      ),
      localizationsDelegates: const [FLocalizations.delegate],
      supportedLocales: FLocalizations.supportedLocales,
      routerConfig: _router,
      // The Navigator (child) fills the whole window so dialog barriers cover
      // everything. The window drag bar lives inside each page (see the router)
      // rather than wrapping the Navigator, so dialogs render above it.
      builder: (context, child) => RepaintBoundary(
        key: _rootKey,
        child: FTheme(data: appTheme, child: child!),
      ),
    );
  }
}
