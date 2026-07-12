import 'package:provider/provider.dart';
import 'package:provider/single_child_widget.dart';

import '../data/db/persistence.dart';
import '../data/repositories/song_practice_repository.dart';
import '../data/repositories/technique_repository.dart';
import '../data/services/app_settings_service.dart';
import '../data/services/audio_file_service.dart';
import '../data/services/local_storage_service.dart';
import '../data/services/source_separation_service.dart';
import '../data/services/spectrogram_service.dart';
import '../data/services/technique_media_service.dart';
import '../data/services/update_check_service.dart';
import '../data/services/waveform_service.dart';
import 'environment.dart';

/// The app's dependency-injection graph. Stateless services and repositories
/// are registered once at the top of the tree; per-screen ViewModels are
/// created in the router and read these via `context.read`. Stateful, per-view
/// services (playback, recording) are owned by the detail ViewModel itself.
///
/// The two repositories come from a [Persistence] layer opened once at
/// startup (see main); everything else is created here.
///
/// [separationOverride] lets a test swap in a fake separation engine (the real
/// one shells out to a Python sidecar); production passes nothing.
List<SingleChildWidget> buildProviders(
  Persistence persistence, {
  SourceSeparationService? separationOverride,
}) => [
  Provider<LocalStorageService>(create: (_) => LocalStorageService()),
  ChangeNotifierProvider<AppSettingsService>(
    create: (context) =>
        AppSettingsService(storage: context.read<LocalStorageService>())
          ..load(),
  ),
  Provider<SongPracticeRepository>.value(value: persistence.songRepo),
  Provider<TechniqueRepository>.value(value: persistence.techRepo),
  // The persistence layer itself, for the maintenance surfaces (Settings'
  // "Repair library" runs Persistence.repairLibrary()).
  Provider<Persistence>.value(value: persistence),
  // One app-wide update checker so the home banner and the Settings row share
  // the same result and the same once-per-run auto-check state.
  ChangeNotifierProvider<UpdateCheckService>(
    create: (_) => UpdateCheckService(),
  ),
  Provider<TechniqueMediaService>(
    create: (context) =>
        TechniqueMediaService(storage: context.read<LocalStorageService>()),
  ),
  Provider<AudioFileService>(
    create: (context) =>
        AudioFileService(storage: context.read<LocalStorageService>()),
  ),
  Provider<WaveformService>(
    create: (context) =>
        WaveformService(storage: context.read<LocalStorageService>()),
  ),
  Provider<SpectrogramService>(create: (_) => const SpectrogramService()),
  Provider<SourceSeparationService>(
    create: (context) =>
        separationOverride ??
        createSourceSeparationService(
          AppEnvironment.current,
          context.read<LocalStorageService>(),
        ),
  ),
];

/// Picks the separation engine for [env]. Prod ships no engine yet, so it gets
/// [NullSourceSeparationService]: it reports unavailable without ever spawning
/// a process — a released build must never probe a user's machine for Python
/// interpreters. Dev/test keep the Python-sidecar prototype.
SourceSeparationService createSourceSeparationService(
  AppEnvironment env,
  LocalStorageService storage,
) {
  if (env.isProd) return const NullSourceSeparationService();
  return ProcessSourceSeparationService(
    storage: storage,
    // macOS GUI apps launched from Finder get a minimal PATH, so `python3`
    // may not resolve. Bake the interpreter at build time when needed:
    //   flutter run --dart-define=VOCARA_PYTHON=/opt/miniconda3/bin/python3
    // (For a shipped build this whole engine is replaced by a bundled
    // binary, so the host Python is irrelevant.)
    pythonExecutable: const String.fromEnvironment(
      'VOCARA_PYTHON',
      defaultValue: 'python3',
    ),
  );
}
