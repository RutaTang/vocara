import 'dart:io' show Process, exit;
import 'dart:ui' show AppExitResponse;

import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:media_kit/media_kit.dart';
import 'package:path_provider/path_provider.dart';
import 'package:provider/provider.dart';
import 'package:window_manager/window_manager.dart';

import 'app.dart';
import 'config/dependencies.dart';
import 'config/environment.dart';
import 'data/db/persistence.dart';
import 'data/services/diagnostics_log.dart';
import 'data/services/recording_finalizer.dart';
import 'data/services/source_separation_service.dart';
import 'ui/core/widgets/fatal_open_screen.dart';

/// Channel to the native window (see MainFlutterWindow.swift): the window starts
/// transparent and is revealed only after Flutter's first frame, so there's no
/// white-frame flash on launch.
const _windowChannel = MethodChannel('vocara/window');

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();

  // Diagnostics first: from here on, every uncaught Flutter/platform error is
  // appended to the rotating on-disk log (then forwarded to the default
  // handlers), so a field failure leaves a trace exportArchive can bundle.
  await DiagnosticsLog.instance.attachGlobalHooks();

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
  // The red close button is only one quit path: Cmd+Q, the app-menu Quit, a
  // Dock quit and logout all go through NSApplication.terminate:, which never
  // reaches the window-close listener above. FlutterAppDelegate implements
  // macOS's cancellable-termination flow (applicationShouldTerminate defers the
  // terminate and asks the framework), so this exit request fires on all of
  // those paths — including windowManager.destroy(), which also calls
  // terminate. Finalize any in-flight recording, then allow the exit.
  // Force-quit / kill -9 is not interceptable, but the recording WAV's header
  // is patched in place every ~1 s while recording, and the next boot recovers
  // the orphaned take (Persistence.recoverStagedRecordings) instead of
  // sweeping it.
  _exitGuard = AppLifecycleListener(
    onExitRequested: () async {
      // Kill any in-flight separation sidecar first (synchronous SIGKILL):
      // it is a child process that macOS reparents rather than kills on app
      // exit, so without this it would keep burning CPU/RAM for minutes.
      // Its orphaned .tmp output is swept on the next launch.
      SeparationQuitGuard.instance.killAll();
      await RecordingFinalizer.instance.finalizeAll();
      return AppExitResponse.exit;
    },
  );

  // Open the drift/SQLite store (db file, device node id, asset pool) once.
  // A fatal open MUST still end in a visible window: the native window stays
  // transparent until the show call below, so awaiting open() bare would turn
  // any open failure into an invisible, apparently-hung launch. Refusals and
  // failures get a dedicated minimal error window instead.
  final Persistence persistence;
  try {
    persistence = await Persistence.open(
      onWaitingForLibrary: () =>
          _runFatalWindow(FatalOpenScreen.waitingForLibrary(onQuit: _quit)),
    );
  } on NewerLibraryException catch (e) {
    DiagnosticsLog.instance.log('boot', 'open refused: $e');
    _runFatalWindow(FatalOpenScreen.newerLibrary(onQuit: _quit));
    return;
  } on LibraryInUseException catch (e) {
    DiagnosticsLog.instance.log('boot', 'open refused: $e');
    _runFatalWindow(FatalOpenScreen.libraryInUse(onQuit: _quit));
    return;
  } catch (e, stack) {
    DiagnosticsLog.instance.log('boot', 'open failed: $e\n$stack');
    _runFatalWindow(
      FatalOpenScreen.openFailed(
        detail: '$e',
        onRevealLibrary: _revealLibraryFolder,
        onQuit: _quit,
      ),
    );
    return;
  }

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

/// Runs the minimal fatal-error app and reveals the native window (see the
/// show-channel comment above: without the reveal, a fatal open would leave
/// the window transparent forever).
void _runFatalWindow(FatalOpenScreen screen) {
  runApp(FatalOpenApp(screen: screen));
  WidgetsBinding.instance.addPostFrameCallback((_) {
    _windowChannel.invokeMethod<void>('show');
  });
}

/// Opens the app's library folder (the app support directory holding the
/// database and asset pool) in Finder, for backups or support.
Future<void> _revealLibraryFolder() async {
  try {
    final dir = await getApplicationSupportDirectory();
    await Process.run('open', [dir.path]);
  } catch (_) {
    // Best-effort: the fatal window stays up either way.
  }
}

/// Quits from the fatal window. destroy() routes through the normal
/// cancellable-termination flow (the exit guard's finalizeAll is a no-op with
/// no recording in flight); a hard exit is the last resort.
Future<void> _quit() async {
  try {
    await windowManager.destroy();
  } catch (_) {
    exit(0);
  }
}

/// Keeps the exit-request guard alive for the app's lifetime (registered in
/// [main], never disposed: it must outlive every quit path). Write-only by
/// design — the reference itself is the point.
// ignore: unused_element
late final AppLifecycleListener _exitGuard;

/// Finalizes any in-flight recording before the window actually closes. With
/// [WindowManager.setPreventClose] on, the OS close is deferred to here.
class _QuitGuard with WindowListener {
  // Re-entrancy guard: with preventClose on, every close click fires
  // onWindowClose again, and a second run must not race a second destroy()
  // (or re-run finalizers) while the first save is still in flight.
  bool _closing = false;

  @override
  void onWindowClose() async {
    if (_closing) return;
    _closing = true;
    // See onExitRequested: the sidecar would outlive the process otherwise.
    SeparationQuitGuard.instance.killAll();
    await RecordingFinalizer.instance.finalizeAll();
    await windowManager.destroy();
  }
}
