// Home → a practice → the transport bar's back chevron → Home → the SAME or
// another practice must open again.
//
// Home awaits the detail push: the future's completion is what reloads the
// list and releases its double-tap open guard. go_router completes a push
// future only when the route is POPPED — a `context.go(home)` drops the pushed
// route without completing it, so the guard stayed set for the life of the
// Home page and every later open (a card tap, or New practice → Create) was
// silently swallowed until the app restarted.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:forui/forui.dart';
import 'package:go_router/go_router.dart';
import 'package:provider/provider.dart';
import 'package:vocara/data/db/persistence.dart';
import 'package:vocara/data/repositories/song_practice_repository.dart';
import 'package:vocara/data/repositories/technique_repository.dart';
import 'package:vocara/data/services/app_settings_service.dart';
import 'package:vocara/data/services/audio_file_service.dart';
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/data/services/playback_engine.dart';
import 'package:vocara/data/services/source_separation_service.dart';
import 'package:vocara/data/services/spectrogram_service.dart';
import 'package:vocara/data/services/update_check_service.dart';
import 'package:vocara/data/services/waveform_service.dart';
import 'package:vocara/domain/models/audio_fx.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/routing/routes.dart';
import 'package:vocara/ui/home/view_model/home_view_model.dart';
import 'package:vocara/ui/home/widgets/home_screen.dart';
import 'package:vocara/ui/home/widgets/practice_project_card.dart';
import 'package:vocara/ui/song_practice_detail/view_model/song_practice_detail_view_model.dart';
import 'package:vocara/ui/song_practice_detail/widgets/song_practice_detail_screen.dart';
import 'package:vocara/utils/result.dart';

class _FakeEngine extends Fake implements PlaybackEngine {
  @override
  Stream<Duration> get position => const Stream.empty();
  @override
  Stream<bool> get playing => const Stream.empty();
  @override
  Duration get currentPosition => Duration.zero;
  @override
  bool get isPlaying => false;
  @override
  bool get supportsEffects => false;
  @override
  void setLoop(bool enabled, int startMs, int endMs) {}
  @override
  Future<void> sync(
    List<TrackPlayback> tracks,
    List<BusPlayback> buses,
  ) async {}
  @override
  Future<void> play() async {}
  @override
  Future<void> pause() async {}
  @override
  Future<void> seek(Duration position) async {}
  @override
  Future<void> setVolume(
    String id,
    double volume, {
    bool muted = false,
  }) async {}
  @override
  void setTrackPan(String id, double pan) {}
  @override
  void setMasterVolume(double gain) {}
  @override
  void setTrackFx(String trackId, TrackFx fx) {}
  @override
  void setBusFx(String busId, TrackFx fx) {}
  @override
  void setBusVolume(String busId, double volume, {bool muted = false}) {}
  @override
  void setBusPan(String busId, double pan) {}
  @override
  double busGainReductionDb(String busId) => 0;
  @override
  void setTrackSendLevel(String trackId, int slot, double levelDb) {}
  @override
  void setBusSendLevel(String busId, int slot, double levelDb) {}
  @override
  void setReverb(ReverbSettings reverb) {}
  @override
  double trackGainReductionDb(String trackId) => 0;
  @override
  Future<void> dispose() async {}
}

void main() {
  testWidgets('the transport back chevron returns Home ready to open the '
      'next practice', (tester) async {
    tester.view.physicalSize = const Size(2560, 1440);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);

    final persistence = Persistence.memory();
    await tester.runAsync(() async {
      await persistence.songRepo.createPractice('First');
      await persistence.songRepo.createPractice('Second');
    });
    final storage = LocalStorageService();
    final settings = AppSettingsService(
      storage: storage,
      enumerateOutputs: () => const [],
      enumerateInputs: () => const [],
    );
    final router = GoRouter(
      routes: [
        GoRoute(
          path: Routes.home,
          builder: (context, _) => ChangeNotifierProvider(
            create: (context) => HomeViewModel(
              repository: context.read<SongPracticeRepository>(),
            ),
            child: const HomeScreen(),
          ),
        ),
        GoRoute(
          path: Routes.songPracticePattern,
          builder: (context, state) => ChangeNotifierProvider(
            create: (context) => SongPracticeDetailViewModel(
              repository: context.read<SongPracticeRepository>(),
              techniqueRepository: context.read<TechniqueRepository>(),
              audioFileService: AudioFileService(storage: storage),
              waveformService: WaveformService(storage: storage),
              spectrogramService: const SpectrogramService(),
              separationService: FakeSourceSeparationService(available: false),
              appSettings: settings,
              practiceId: state.pathParameters['id']!,
              playbackEngine: _FakeEngine(),
              probeDuration: (_) => null,
            ),
            child: const SongPracticeDetailScreen(),
          ),
        ),
      ],
    );
    await tester.pumpWidget(
      MultiProvider(
        providers: [
          Provider<SongPracticeRepository>.value(value: persistence.songRepo),
          Provider<TechniqueRepository>.value(value: persistence.techRepo),
          ChangeNotifierProvider(
            create: (_) => UpdateCheckService(fetch: (_) async => '{}'),
          ),
        ],
        child: MaterialApp.router(
          routerConfig: router,
          // The em-wide test font overflows the detail's fixed-width rows that
          // real glyphs fit (see lyrics_panel_test); shrink it to ship width.
          builder: (context, child) => MediaQuery(
            data: MediaQuery.of(
              context,
            ).copyWith(textScaler: const TextScaler.linear(0.6)),
            child: child!,
          ),
        ),
      ),
    );

    Future<void> frames([int n = 25]) async {
      for (var i = 0; i < n; i++) {
        await tester.pump(const Duration(milliseconds: 100));
      }
    }

    Future<void> openAndComeBack(
      String title, {
      Future<void> Function()? whileOpen,
    }) async {
      await tester.tap(find.widgetWithText(PracticeProjectCard, title));
      // The opening dialog holds for ~1.5 s before the push.
      await frames();
      await tester.runAsync(
        () => Future<void>.delayed(const Duration(milliseconds: 50)),
      );
      await frames(5);
      expect(
        find.byType(SongPracticeDetailScreen),
        findsOneWidget,
        reason: 'tapping "$title" opens its detail',
      );
      if (whileOpen != null) await tester.runAsync(whileOpen);
      await tester.tap(
        find
            .descendant(
              of: find.byType(SongPracticeDetailScreen),
              matching: find.byIcon(FIcons.chevronLeft),
            )
            .first,
      );
      await frames(10);
      expect(find.byType(SongPracticeDetailScreen), findsNothing);
      expect(find.byType(HomeScreen), findsOneWidget);
    }

    await frames(3);
    expect(find.byType(PracticeProjectCard), findsNWidgets(2));
    await openAndComeBack('First');
    // The same awaited push is what refreshes the list on the way back.
    await openAndComeBack(
      'Second',
      whileOpen: () async {
        final all =
            (await persistence.songRepo.listPractices()
                    as Ok<List<SongPractice>>)
                .value;
        final second = all.firstWhere((p) => p.title == 'Second');
        await persistence.songRepo.savePractice(
          second.copyWith(title: 'Second, renamed'),
        );
      },
    );
    expect(
      find.widgetWithText(PracticeProjectCard, 'Second, renamed'),
      findsOneWidget,
      reason: 'returning from a practice reloads the Home list',
    );
    await openAndComeBack('First');

    await tester.pumpWidget(const SizedBox());
    await tester.runAsync(() => persistence.db.close());
  });
}
