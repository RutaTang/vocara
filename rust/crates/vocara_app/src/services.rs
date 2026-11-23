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

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::app::library::Library;
use vocara_model::config::environment::AppEnvironment;
use vocara_services::audio_file_service::{AudioFileService, AudioFiles};
use vocara_services::local_storage_service::LocalStorageService;
use vocara_services::source_separation_service::{
    FakeSourceSeparationService, NullSourceSeparationService, ProcessSourceSeparationService,
    SourceSeparationService,
};
use vocara_services::spectrogram_service::{SpectrogramAnalyses, SpectrogramService};
use vocara_services::technique_media_service::{TechniqueMediaFiles, TechniqueMediaService};
use vocara_services::update_check_service::{FetchError, UpdateCheckService};
use vocara_store::repositories::{SongPracticeRepository, TechniqueRepository};

/// Everything a screen reads from the app (`context.read<...>()`).
#[derive(Clone)]
pub struct Services {
    /// The persistence layer opened once at startup (`Provider<Persistence>`):
    /// the store behind both repositories, `remoteApplied`, and the
    /// maintenance surfaces (Settings' "Repair library").
    pub library: Arc<Library>,

    /// The build environment the services were chosen for.
    pub environment: AppEnvironment,

    /// `Provider<SongPracticeRepository>.value(persistence.songRepo)`.
    pub song_repo: Arc<dyn SongPracticeRepository>,

    /// `Provider<TechniqueRepository>.value(persistence.techRepo)`.
    pub technique_repo: Arc<dyn TechniqueRepository>,

    /// `Provider<LocalStorageService>`.
    pub local_storage: Arc<LocalStorageService>,

    /// `ChangeNotifierProvider<UpdateCheckService>`: ONE app-wide checker, so
    /// the home banner and the Settings row share the result and the
    /// once-per-run auto-check state.
    pub update_check: Arc<UpdateCheckService>,

    /// `Provider<TechniqueMediaService>`.
    pub technique_media: Arc<dyn TechniqueMediaFiles>,

    /// `Provider<AudioFileService>`.
    pub audio_files: Arc<dyn AudioFiles>,

    /// `Provider<SpectrogramService>`.
    pub spectrograms: Arc<dyn SpectrogramAnalyses>,

    /// `Provider<SourceSeparationService>` from
    /// [`create_source_separation_service`], or a test's override.
    pub separation: Arc<dyn SourceSeparationService>,
    //
    // ---- To add as the services port lands ----
    //
    // `ChangeNotifierProvider<AppSettingsService>`, whose `load()` was started
    // un-awaited at creation (the first frame must not wait on a settings
    // read; `load` adopts only fields the user has not already set) with its
    // failure logged as `settings` / `load failed: <type>`:
    //     pub app_settings: Arc<AppSettingsService>,
    // `Provider<WaveformService>`: the Rust waveform service is free
    // functions over `local_storage` (`vocara_services::waveform_service`).
}

impl std::fmt::Debug for Services {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Services")
            .field("environment", &self.environment)
            .finish_non_exhaustive()
    }
}

/// Builds the services over an opened library: [`Services::new`] in the app,
/// [`Services::detached`] in tests.
pub type ServicesFactory = fn(Arc<Library>, AppEnvironment) -> Services;

impl Services {
    /// The services over an opened library (`buildProviders(persistence)`).
    pub fn new(library: Arc<Library>, environment: AppEnvironment) -> Self {
        let local_storage = Arc::new(LocalStorageService::new(environment.bundle_id()));
        Self::with_parts(
            library,
            environment,
            local_storage.clone(),
            Arc::new(UpdateCheckService::new()),
            create_source_separation_service(environment, local_storage),
        )
    }

    /// The services a test boots with: storage in a fresh temporary folder
    /// (never the real application support directory), an update check that
    /// is always offline, and a separation fake that reports unavailable —
    /// what Dart's widget tests overrode (`separationOverride`, the stub
    /// fetcher).
    pub fn detached(library: Arc<Library>, environment: AppEnvironment) -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let root: PathBuf = std::env::temp_dir().join(format!(
            "vocara-test-storage-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        Self::with_parts(
            library,
            environment,
            Arc::new(LocalStorageService::with_root(root)),
            Arc::new(UpdateCheckService::with_fetcher(
                Box::new(|_| Err(FetchError("offline".to_string()))),
                UpdateCheckService::APP_VERSION,
            )),
            Arc::new(FakeSourceSeparationService { available: false }),
        )
    }

    /// The services with the given storage, update check and separation;
    /// the rest built over [local_storage] as `buildProviders` did.
    pub fn with_parts(
        library: Arc<Library>,
        environment: AppEnvironment,
        local_storage: Arc<LocalStorageService>,
        update_check: Arc<UpdateCheckService>,
        separation: Arc<dyn SourceSeparationService>,
    ) -> Self {
        Self {
            song_repo: library.song_repo.clone(),
            technique_repo: library.technique_repo.clone(),
            technique_media: Arc::new(TechniqueMediaService::new(local_storage.clone())),
            audio_files: Arc::new(AudioFileService::new(local_storage.clone())),
            spectrograms: Arc::new(SpectrogramService::new()),
            local_storage,
            update_check,
            separation,
            library,
            environment,
        }
    }
}

/// Picks the separation engine for [env] (`createSourceSeparationService`).
/// Prod ships no engine yet, so it gets [`NullSourceSeparationService`]: it
/// reports unavailable without ever spawning a process — a released build
/// must never probe a user's machine for Python interpreters. Dev/test keep
/// the Python-sidecar prototype, its interpreter baked at build time from
/// `VOCARA_PYTHON` (a GUI app launched from Finder gets a minimal PATH, so
/// `python3` may not resolve), default `python3`.
pub fn create_source_separation_service(
    env: AppEnvironment,
    storage: Arc<LocalStorageService>,
) -> Arc<dyn SourceSeparationService> {
    if env.is_prod() {
        return Arc::new(NullSourceSeparationService);
    }
    Arc::new(ProcessSourceSeparationService::new(
        storage,
        option_env!("VOCARA_PYTHON").unwrap_or("python3"),
        None,
    ))
}
