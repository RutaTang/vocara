import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:media_kit/media_kit.dart';
import 'package:provider/provider.dart';
import 'package:window_manager/window_manager.dart';

import 'app.dart';
import 'config/dependencies.dart';
import 'config/environment.dart';
import 'data/db/persistence.dart';
import 'data/services/recording_finalizer.dart';

/// Channel to the native window (see MainFlutterWindow.swift): the window starts
/// transparent and is revealed only after Flutter's first frame, so there's no
/// white-frame flash on launch.
const _windowChannel = MethodChannel('vocara/window');

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();

  // Initialize media_kit, which backs the inline audio/video players used for
  // technique example clips.
  MediaKit.ensureInitialized();

  // Frameless window: hide the OS title bar but keep the traffic lights; the
  // top bar (WindowDragBar) provides an Electron-style drag region. The window
  // is revealed natively after the first frame (below), not here.
  await windowManager.ensureInitialized();
  await windowManager.setTitleBarStyle(TitleBarStyle.hidden);
  // Suffix the window title in dev/test (blank in prod) so the running
  // environment is identifiable in ⌘-Tab / Mission Control.
  await windowManager.setTitle(AppEnvironment.current.windowTitle);
  // Intercept the OS close so an in-flight recording is finalized (saved) before
  // the process is torn down, instead of being lost as a 0-length take.
  await windowManager.setPreventClose(true);
  windowManager.addListener(_QuitGuard());

  // Open the drift/SQLite store (db file, device node id, asset pool) once.
  final persistence = await Persistence.open();

  runApp(
    MultiProvider(
      providers: buildProviders(persistence),
      child: const VocaraApp(),
    ),
  );

  // Reveal the native window once Flutter has painted its first frame.
  WidgetsBinding.instance.addPostFrameCallback((_) {
    _windowChannel.invokeMethod<void>('show');
  });
}

/// Finalizes any in-flight recording before the window actually closes. With
/// [WindowManager.setPreventClose] on, the OS close is deferred to here.
class _QuitGuard with WindowListener {
  @override
  void onWindowClose() async {
    await RecordingFinalizer.instance.finalizeAll();
    await windowManager.destroy();
  }
}
