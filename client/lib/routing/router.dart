import 'package:flutter/widgets.dart';
import 'package:go_router/go_router.dart';
import 'package:provider/provider.dart';

import '../data/db/persistence.dart';
import '../data/repositories/song_practice_repository.dart';
import '../data/repositories/technique_repository.dart';
import '../data/services/app_settings_service.dart';
import '../data/services/audio_file_service.dart';
import '../data/services/source_separation_service.dart';
import '../data/services/spectrogram_service.dart';
import '../data/services/technique_media_service.dart';
import '../data/services/waveform_service.dart';
import '../ui/core/widgets/window_drag_bar.dart';
import '../ui/home/view_model/home_view_model.dart';
import '../ui/home/widgets/home_screen.dart';
import '../ui/settings/settings_screen.dart';
import '../ui/song_practice_detail/view_model/song_practice_detail_view_model.dart';
import '../ui/song_practice_detail/widgets/song_practice_detail_screen.dart';
import '../ui/technique_library/technique_library_screen.dart';
import '../ui/technique_library/view_model/technique_detail_view_model.dart';
import '../ui/technique_library/view_model/technique_library_view_model.dart';
import '../ui/technique_library/widgets/technique_detail_screen.dart';
import 'routes.dart';

/// Wraps a page in the window chrome: the frameless window's drag bar sits at
/// the top of every page (inside the Navigator) rather than wrapping it, so a
/// dialog pushed on the Navigator renders above the drag bar and its barrier
/// covers the whole window.
Widget _chrome(Widget page) => Column(
  children: [
    const WindowDragBar(),
    Expanded(child: page),
  ],
);

/// Builds the app router. Each route owns its ViewModel via a
/// [ChangeNotifierProvider] (1 View ↔ 1 ViewModel), reading shared
/// repositories/services from the surrounding DI graph.
GoRouter createRouter() {
  // Dev-only deep-link for screenshots: `--dart-define=START=/song/<id>`.
  const start = String.fromEnvironment('START');
  return GoRouter(
    initialLocation: start.isNotEmpty ? start : Routes.home,
    routes: [
      GoRoute(
        path: Routes.home,
        builder: (context, state) => _chrome(
          ChangeNotifierProvider(
            create: (context) => HomeViewModel(
              repository: context.read<SongPracticeRepository>(),
              // Freshness reloads when sync applies remote rows; with no sync
              // configured the stream simply never fires.
              remoteApplied: context.read<Persistence>().remoteApplied,
            ),
            child: const HomeScreen(),
          ),
        ),
      ),
      GoRoute(
        path: Routes.songPracticePattern,
        builder: (context, state) {
          final id = state.pathParameters['id']!;
          return _chrome(
            ChangeNotifierProvider(
              create: (context) => SongPracticeDetailViewModel(
                repository: context.read<SongPracticeRepository>(),
                techniqueRepository: context.read<TechniqueRepository>(),
                audioFileService: context.read<AudioFileService>(),
                waveformService: context.read<WaveformService>(),
                spectrogramService: context.read<SpectrogramService>(),
                separationService: context.read<SourceSeparationService>(),
                appSettings: context.read<AppSettingsService>(),
                practiceId: id,
                remoteApplied: context.read<Persistence>().remoteApplied,
              ),
              child: const SongPracticeDetailScreen(),
            ),
          );
        },
      ),
      GoRoute(
        path: Routes.settings,
        builder: (context, state) => _chrome(const SettingsScreen()),
      ),
      GoRoute(
        path: Routes.techniqueLibrary,
        builder: (context, state) => _chrome(
          ChangeNotifierProvider(
            create: (context) => TechniqueLibraryViewModel(
              repository: context.read<TechniqueRepository>(),
              mediaService: context.read<TechniqueMediaService>(),
              remoteApplied: context.read<Persistence>().remoteApplied,
            ),
            child: const TechniqueLibraryScreen(),
          ),
        ),
      ),
      GoRoute(
        path: Routes.techniqueDetailPattern,
        builder: (context, state) {
          final id = state.pathParameters['id']!;
          return _chrome(
            ChangeNotifierProvider(
              create: (context) => TechniqueDetailViewModel(
                repository: context.read<TechniqueRepository>(),
                mediaService: context.read<TechniqueMediaService>(),
                techniqueId: id,
                appSettings: context.read<AppSettingsService>(),
                remoteApplied: context.read<Persistence>().remoteApplied,
              ),
              child: const TechniqueDetailScreen(),
            ),
          );
        },
      ),
    ],
  );
}
