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
use vocara_model::diagnostics::DiagnosticsLog;
use vocara_services::app_settings_service::{AppSettingsService, DeviceQueries};
use vocara_services::audio_file_service::{AudioFileService, AudioFiles};
use vocara_services::local_storage_service::LocalStorageService;
use vocara_services::self_update::SelfUpdate;
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
    /// once-per-run auto-check state, and the answer to "check
    /// automatically?".
    pub update_check: Arc<UpdateCheckService>,

    /// The self-update: ONE per app run, so the Home banner, the update
    /// dialog and Settings all show the same download, and the app quits
    /// for its relaunch once it is staged.
    pub self_update: Arc<SelfUpdate>,

    /// `Provider<TechniqueMediaService>`.
    pub technique_media: Arc<dyn TechniqueMediaFiles>,

    /// `Provider<AudioFileService>`.
    pub audio_files: Arc<dyn AudioFiles>,

    /// `Provider<SpectrogramService>`.
    pub spectrograms: Arc<dyn SpectrogramAnalyses>,

    /// `Provider<SourceSeparationService>` from
    /// [`create_source_separation_service`], or a test's override.
    pub separation: Arc<dyn SourceSeparationService>,

    /// `ChangeNotifierProvider<AppSettingsService>`, its `load()` started
    /// without waiting at creation: the first frame must not wait on a
    /// settings read, and `load` adopts only fields the user has not already
    /// set.
    pub app_settings: Arc<AppSettingsService>,
    //
    // `Provider<WaveformService>` has no field: the Rust waveform service is
    // free functions (`vocara_services::waveform_service`).
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
        let app_settings = Arc::new(AppSettingsService::new(local_storage.clone()));
        start_settings_load(&app_settings);
        let update_check = Arc::new(UpdateCheckService::new(local_storage.clone()));
        start_update_preference_load(&update_check);
        Self::with_parts(
            library,
            environment,
            local_storage.clone(),
            update_check,
            Arc::new(SelfUpdate::production(local_storage.clone())),
            create_source_separation_service(environment, local_storage),
            app_settings,
        )
    }

    /// The services a test boots with: storage in a fresh temporary folder
    /// (never the real application support directory), an update check that
    /// is always offline (its "check automatically?" unanswered, kept in
    /// memory), a self-update that cannot replace a test binary, and a
    /// separation fake that reports unavailable — what Dart's widget tests
    /// overrode (`separationOverride`, the stub fetcher).
    pub fn detached(library: Arc<Library>, environment: AppEnvironment) -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let root: PathBuf = std::env::temp_dir().join(format!(
            "vocara-test-storage-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let local_storage = Arc::new(LocalStorageService::with_root(root));
        let app_settings = Arc::new(AppSettingsService::with_queries(
            local_storage.clone(),
            DeviceQueries {
                enumerate_outputs: Box::new(Vec::new),
                enumerate_inputs: Box::new(Vec::new),
                input_channel_count: Box::new(|_| 0),
            },
        ));
        Self::with_parts(
            library,
            environment,
            local_storage,
            Arc::new(UpdateCheckService::with_fetcher(
                Box::new(|_| Err(FetchError("offline".to_string()))),
                UpdateCheckService::APP_VERSION,
            )),
            Arc::new(SelfUpdate::unavailable(
                "Vocara is not running from an installed app",
            )),
            Arc::new(FakeSourceSeparationService { available: false }),
            app_settings,
        )
    }

    /// The services with the given storage, update check, self-update,
    /// separation and settings; the rest built over [local_storage] as
    /// `buildProviders` did.
    pub fn with_parts(
        library: Arc<Library>,
        environment: AppEnvironment,
        local_storage: Arc<LocalStorageService>,
        update_check: Arc<UpdateCheckService>,
        self_update: Arc<SelfUpdate>,
        separation: Arc<dyn SourceSeparationService>,
        app_settings: Arc<AppSettingsService>,
    ) -> Self {
        Self {
            song_repo: library.song_repo.clone(),
            technique_repo: library.technique_repo.clone(),
            technique_media: Arc::new(TechniqueMediaService::new(local_storage.clone())),
            audio_files: Arc::new(AudioFileService::new(local_storage.clone())),
            spectrograms: Arc::new(SpectrogramService::new()),
            local_storage,
            update_check,
            self_update,
            separation,
            app_settings,
            library,
            environment,
        }
    }
}

/// Reads the answer to "check for updates automatically?" on its own thread,
/// so Settings' switch shows it without reading the disk on the UI thread
/// (until it lands the switch reads off, and nothing is checked: only a yes
/// lets the automatic check run).
fn start_update_preference_load(update_check: &Arc<UpdateCheckService>) {
    let update_check = update_check.clone();
    let spawned = std::thread::Builder::new()
        .name("update-preference-load".into())
        .spawn(move || {
            let _ = update_check.auto_check_preference();
        });
    if let Err(e) = spawned {
        DiagnosticsLog::instance().log("update", &format!("preference load failed: {e}"));
    }
}

/// Starts [settings]' `load()` on its own thread and does not wait for it.
/// A failure is logged as Dart's `catchError` logged it (`settings` /
/// `load failed: <type>`), never surfaced as a crash.
fn start_settings_load(settings: &Arc<AppSettingsService>) {
    let settings = settings.clone();
    let spawned = std::thread::Builder::new()
        .name("settings-load".into())
        .spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| settings.load())).is_err() {
                DiagnosticsLog::instance().log("settings", "load failed: panic");
            }
        });
    if let Err(e) = spawned {
        DiagnosticsLog::instance().log("settings", &format!("load failed: {e}"));
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
