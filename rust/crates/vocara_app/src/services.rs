//! The app's services (port of `config/dependencies.dart`'s
//! `buildProviders`): what Dart registered once at the top of the widget tree
//! and every screen read with `context.read`, here one [`Services`] value the
//! app owns and lends to each screen (`&Services` in `update`; clone it — it
//! is all `Arc`s — into a task that needs it off the UI thread).
//!
//! Stateful, per-view services (playback, recording) are owned by the screen
//! that uses them, as the Dart detail view model owned them.
//!
//! Only what exists is here. The rest is marked below, each with the Dart
//! provider it replaces; the services port adds each field as its service
//! lands.

use std::sync::Arc;

use crate::app::environment::AppEnvironment;
use crate::app::library::Library;

/// Everything a screen reads from the app (`context.read<...>()`).
#[derive(Debug, Clone)]
pub struct Services {
    /// The persistence layer opened once at startup (`Provider<Persistence>`):
    /// the store behind both repositories, `remoteApplied`, and the
    /// maintenance surfaces (Settings' "Repair library").
    pub library: Arc<Library>,

    /// The build environment the services were chosen for.
    pub environment: AppEnvironment,
    //
    // ---- To add as the services port lands (vocara_store / vocara_services) ----
    //
    // `Provider<SongPracticeRepository>.value(persistence.songRepo)`:
    //     pub song_repo: Arc<dyn SongPracticeRepository>,
    // `Provider<TechniqueRepository>.value(persistence.techRepo)`:
    //     pub technique_repo: Arc<dyn TechniqueRepository>,
    // `Provider<LocalStorageService>`:
    //     pub local_storage: Arc<LocalStorageService>,
    // `ChangeNotifierProvider<AppSettingsService>`, whose `load()` was started
    // un-awaited at creation (the first frame must not wait on a settings
    // read; `load` adopts only fields the user has not already set) with its
    // failure logged as `settings` / `load failed: <type>`:
    //     pub app_settings: Arc<AppSettingsService>,
    // `ChangeNotifierProvider<UpdateCheckService>` — ONE app-wide checker, so
    // the home banner and the Settings row share the result and the
    // once-per-run auto-check state:
    //     pub update_check: Arc<UpdateCheckService>,
    // `Provider<TechniqueMediaService>`:
    //     pub technique_media: Arc<TechniqueMediaService>,
    // `Provider<AudioFileService>`:
    //     pub audio_files: Arc<AudioFileService>,
    // `Provider<WaveformService>`:
    //     pub waveforms: Arc<WaveformService>,
    // `Provider<SpectrogramService>`:
    //     pub spectrograms: Arc<SpectrogramService>,
    // `Provider<SourceSeparationService>` from
    // `createSourceSeparationService(environment, localStorage)` (prod gets
    // the Null engine, which never spawns a process; dev/test the Python
    // sidecar, its interpreter from VOCARA_PYTHON, default `python3`), or a
    // test's override:
    //     pub separation: Arc<dyn SourceSeparationService>,
}

impl Services {
    /// The services over an opened library (`buildProviders(persistence)`).
    pub fn new(library: Arc<Library>, environment: AppEnvironment) -> Self {
        Self {
            library,
            environment,
        }
    }
}
