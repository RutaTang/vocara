//! The self-update: one click on "Update now" downloads the release's disk
//! image, checks it, swaps it in for the running app and relaunches Vocara.
//! A port of the owner's Clew updater (`crates/clew-core/src/update.rs`,
//! `src/backend/updater.rs`, `src/app/updater.rs`, `src/macos/install.rs`),
//! as one service the screens share, the way they share the update check.
//!
//! What happens, in order, on a thread of its own ([`SelfUpdate::start`]):
//!
//! 1. **Can this copy replace itself?** ([`Installer::blocker`]) Not from a
//!    development build, from the disk image, from App Translocation's
//!    read-only copy, from a folder this account cannot write, or from a
//!    build with no Developer ID team to anchor an update to. Asked BEFORE
//!    the download, so the answer is a line on screen and the release page,
//!    not a whole image fetched for nothing (or worse, a Vocara that quit for
//!    a swap that cannot happen).
//! 2. **The download** ([`download`]): HTTPS only, from this repository's
//!    release assets only, into a private directory of its own under the
//!    app's data root ([`files`]), at most the size the manifest names, and
//!    its SHA-256 checked against the manifest's. That is integrity, not
//!    trust: the manifest is a file on a web server.
//! 3. **The install** ([`Installer::install`]; macOS: [`install`]): the app on
//!    the image must satisfy a code requirement pinned to the RUNNING app's
//!    own signing identifier and Team ID, with Developer ID certificates and
//!    an Apple anchor — that is what establishes provenance — the image must
//!    carry Apple's notarization ticket, and the bundle must declare exactly
//!    the version offered, newer than the running one. The verified app is
//!    copied into a private staging directory, and a detached helper is
//!    started that waits for Vocara to exit, swaps the bundle (backing up the
//!    old one, restoring it on failure), and relaunches.
//! 4. **The quit** ([`Phase::Relaunching`]): the app quits through its normal
//!    quit path (the quit guard: the separation sidecars stopped, the
//!    recordings finalized), which is what the helper waits for.
//!
//! A swap that fails after Vocara quit puts the previous version back,
//! relaunches it, and leaves the reason in a marker file, which the
//! relaunched Vocara reports once ([`SelfUpdate::startup`]).
//!
//! The service holds the one update an app run can have under way: its
//! [`Phase`], which the Home banner, the update dialog and Settings all draw,
//! and listeners the app redraws (and quits) on. The work itself never runs
//! on the UI thread; nothing here contacts the network but the download the
//! user asked for.

pub mod download;
pub mod files;
pub mod install;
pub mod version;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use vocara_model::diagnostics::DiagnosticsLog;

use crate::local_storage_service::LocalStorageService;
use crate::update_check_service::{is_release_asset_url, UpdateCheckService, UpdateInfo};

pub use download::{HttpsTransport, Transport};
pub use version::{check_candidate_version, ensure_upgrade};

/// Largest update image accepted. A Vocara DMG is tens of MB; the manifest
/// names the exact size, and the download is capped at it.
pub const MAX_UPDATE_BYTES: u64 = 512 * 1024 * 1024;

/// How often the download reports its progress at most: the app redraws on
/// each report.
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// Why the update dialog offers the release page when a release names no
/// image Vocara can install (a manifest from before the self-update, or one
/// whose image entry is unusable).
pub const NO_IMAGE: &str = "this release offers no download Vocara can install by itself";

/// The running app's version: the `CFBundleShortVersionString` of the app
/// bundle Vocara runs from, else — outside a bundle (a `cargo run` build,
/// the tests) — the version it was built as
/// ([`UpdateCheckService::APP_VERSION`]). Read once per process.
///
/// The bundle's version is the one that matters for updates: the release
/// stamps it from the tag, and it is the very field an update's own bundle is
/// judged by ([`check_candidate_version`], whose no-downgrade rule compares
/// against this), so comparing with the crate constant would make that rule
/// silently depend on the two agreeing.
pub fn current_version() -> &'static str {
    static RUNNING: OnceLock<String> = OnceLock::new();
    RUNNING.get_or_init(|| {
        version::running_version(
            running_bundle_version().as_deref(),
            UpdateCheckService::APP_VERSION,
        )
    })
}

/// The `CFBundleShortVersionString` of the bundle Vocara runs from, if it
/// runs from one.
#[cfg(target_os = "macos")]
fn running_bundle_version() -> Option<String> {
    install::macos::running_bundle_version()
}

#[cfg(not(target_os = "macos"))]
fn running_bundle_version() -> Option<String> {
    None
}

/// Where an update stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// Nothing under way.
    Idle,
    /// The image is downloading: `done` of `total` bytes (the size the
    /// manifest names).
    Downloading { done: u64, total: u64 },
    /// Downloaded and its checksum matched: the installer verifies the app
    /// on it and stages the swap.
    Installing,
    /// The swap helper is waiting for Vocara to exit: the app quits now,
    /// through its normal quit path, and the helper relaunches the new
    /// version.
    Relaunching,
    /// The update stopped, and why. Also how a swap that failed after the
    /// last quit is reported, once the relaunched Vocara is up.
    Failed(String),
}

impl Phase {
    /// Whether an update is under way (nothing else may start one).
    pub fn busy(&self) -> bool {
        matches!(
            self,
            Phase::Downloading { .. } | Phase::Installing | Phase::Relaunching
        )
    }
}

/// Whether Vocara can replace itself with a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelfInstall {
    /// It can: "Update now".
    Ready,
    /// Not known yet (the check runs off the UI thread, [`SelfUpdate::prepare`]):
    /// "Update now" is offered, and the attempt itself says if it cannot.
    Unknown,
    /// It cannot, and why: the release page is the way.
    Blocked(String),
}

/// What installs a downloaded, checksum-verified image: on macOS,
/// [`install::macos::MacInstaller`]; a test's fake elsewhere.
pub trait Installer: Send + Sync {
    /// Why this copy of Vocara cannot replace itself, or `None` when it can.
    /// Blocking (it may run `codesign`): never on the UI thread.
    fn blocker(&self) -> Option<String>;

    /// Verifies the image, stages its app and starts the helper that swaps
    /// it in once Vocara exits. Returns once the helper is running; the app
    /// must then quit. Blocking.
    fn install(&self, request: &InstallRequest<'_>) -> Result<(), String>;
}

/// What [`Installer::install`] is asked to install.
#[derive(Debug, Clone, Copy)]
pub struct InstallRequest<'a> {
    /// The downloaded image, its size and checksum verified.
    pub dmg: &'a Path,
    /// The version the user was offered: the image must hold exactly it.
    pub expected: &'a str,
    /// The running version: the image must be newer.
    pub running: &'a str,
    /// The private directory updates live in (staging, the helper's log, the
    /// failure marker).
    pub updates_dir: &'a Path,
}

/// The installer off macOS, where Vocara does not update itself.
#[derive(Debug, Default, Clone, Copy)]
pub struct Unsupported;

impl Installer for Unsupported {
    fn blocker(&self) -> Option<String> {
        Some("Vocara can update itself only on macOS".to_string())
    }

    fn install(&self, _request: &InstallRequest<'_>) -> Result<(), String> {
        Err("Vocara can update itself only on macOS".to_string())
    }
}

/// The installer for this platform.
pub fn platform_installer() -> Arc<dyn Installer> {
    #[cfg(target_os = "macos")]
    {
        Arc::new(install::macos::MacInstaller)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Arc::new(Unsupported)
    }
}

type Listener = Arc<dyn Fn() + Send + Sync>;

/// A registered listener, to remove it with [`SelfUpdate::remove_listener`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ListenerId(u64);

/// The self-update (see the module docs). Cheap to share: clone the `Arc`
/// the app's services hold.
pub struct SelfUpdate {
    inner: Arc<Inner>,
}

struct Inner {
    /// The private directory updates live in (`<data root>/updates`), made
    /// on first use.
    updates_dir: Box<dyn Fn() -> io::Result<PathBuf> + Send + Sync>,
    transport: Arc<dyn Transport>,
    installer: Arc<dyn Installer>,
    /// The version releases are compared with (the bundle's).
    running: String,
    state: Mutex<State>,
    listeners: Mutex<Vec<(ListenerId, Listener)>>,
    next_listener: AtomicU64,
    /// The update threads still running, and the signal each sends as it
    /// ends ([`SelfUpdate::wait_for_workers`]).
    workers: Mutex<usize>,
    workers_done: Condvar,
    /// Whether [`SelfUpdate::startup`] ran.
    started_up: AtomicBool,
}

#[derive(Debug)]
struct State {
    phase: Phase,
    /// The version the attempt under way (or the last one) installs.
    version: Option<String>,
    /// Which attempt the phase belongs to: a cancelled or superseded one's
    /// late reports are dropped.
    generation: u64,
    /// The attempt under way's cancel flag.
    cancel: Option<Arc<AtomicBool>>,
    /// Whether this copy can replace itself: `None` until asked, then the
    /// reason it cannot, if any.
    blocker: Option<Option<String>>,
}

impl SelfUpdate {
    /// A self-update with its pieces given: where updates live, how images
    /// are fetched, what installs them, and the running version.
    pub fn new(
        updates_dir: Box<dyn Fn() -> io::Result<PathBuf> + Send + Sync>,
        transport: Arc<dyn Transport>,
        installer: Arc<dyn Installer>,
        running: &str,
    ) -> Self {
        SelfUpdate {
            inner: Arc::new(Inner {
                updates_dir,
                transport,
                installer,
                running: running.to_string(),
                state: Mutex::new(State {
                    phase: Phase::Idle,
                    version: None,
                    generation: 0,
                    cancel: None,
                    blocker: None,
                }),
                listeners: Mutex::new(Vec::new()),
                next_listener: AtomicU64::new(0),
                workers: Mutex::new(0),
                workers_done: Condvar::new(),
                started_up: AtomicBool::new(false),
            }),
        }
    }

    /// The app's: updates under `<data root>/updates` (the application
    /// support directory `storage` is rooted at — never the shared temp
    /// directory, see [`files`]), images over HTTPS, this platform's
    /// installer, the running bundle's version.
    pub fn production(storage: Arc<LocalStorageService>) -> Self {
        Self::new(
            Box::new(move || Ok(storage.root_dir()?.join(files::UPDATES_DIR))),
            Arc::new(HttpsTransport),
            platform_installer(),
            current_version(),
        )
    }

    /// One that cannot update this copy, for `reason` (the app's tests).
    pub fn unavailable(reason: &str) -> Self {
        struct Blocked(String);
        impl Installer for Blocked {
            fn blocker(&self) -> Option<String> {
                Some(self.0.clone())
            }
            fn install(&self, _request: &InstallRequest<'_>) -> Result<(), String> {
                Err(self.0.clone())
            }
        }
        struct Offline;
        impl Transport for Offline {
            fn open(&self, _url: &str) -> Result<Box<dyn io::Read + Send>, String> {
                Err("offline".to_string())
            }
        }
        Self::new(
            Box::new(|| Err(io::Error::other("no data directory"))),
            Arc::new(Offline),
            Arc::new(Blocked(reason.to_string())),
            UpdateCheckService::APP_VERSION,
        )
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.inner.state()
    }

    /// Where the update stands.
    pub fn phase(&self) -> Phase {
        self.state().phase.clone()
    }

    /// The version the update under way (or the last one) installs.
    pub fn version(&self) -> Option<String> {
        self.state().version.clone()
    }

    /// Whether the app should quit now: the swap helper is waiting for it.
    pub fn wants_quit(&self) -> bool {
        self.state().phase == Phase::Relaunching
    }

    /// Whether Vocara can replace itself with `release`, as far as is known
    /// now (never blocks).
    pub fn self_install(&self, release: &UpdateInfo) -> SelfInstall {
        if release.dmg.is_none() {
            return SelfInstall::Blocked(NO_IMAGE.to_string());
        }
        match &self.state().blocker {
            None => SelfInstall::Unknown,
            Some(None) => SelfInstall::Ready,
            Some(Some(reason)) => SelfInstall::Blocked(reason.clone()),
        }
    }

    /// Finds out whether this copy can replace itself, once
    /// ([`Installer::blocker`]), so the offer can say so before anyone
    /// clicks. Blocking (on macOS it runs `codesign`): call it off the UI
    /// thread, after a check found a release.
    pub fn prepare(&self) {
        if self.state().blocker.is_some() {
            return;
        }
        let blocker = self.inner.installer.blocker();
        self.state().blocker.get_or_insert(blocker);
        self.inner.notify();
    }

    /// The once-per-run start-up work, all local: a swap that failed after
    /// the last run quit for it is reported now (the helper relaunched the
    /// previous version and left the reason behind; this is the first moment
    /// anyone can show it), and the downloads earlier runs left behind are
    /// swept ([`files::sweep_stale_downloads`]). Blocking: off the UI thread.
    pub fn startup(&self) {
        if self.inner.started_up.swap(true, Ordering::SeqCst) {
            return;
        }
        let Ok(updates) = (self.inner.updates_dir)() else {
            return;
        };
        files::sweep_stale_downloads(&updates);
        let Some(reason) = files::take_install_failure(&updates) else {
            return;
        };
        DiagnosticsLog::instance().log("update", "the last update's swap failed");
        {
            let mut s = self.state();
            if s.phase.busy() {
                return;
            }
            s.phase = Phase::Failed(reason);
            s.version = None;
        }
        self.inner.notify();
    }

    /// Starts downloading and installing `release` (see the module docs), on
    /// a thread of its own; `false` when an update is already under way, or
    /// the thread could not start. Never blocks.
    pub fn start(&self, release: &UpdateInfo) -> bool {
        let cancel = Arc::new(AtomicBool::new(false));
        let generation = {
            let mut s = self.state();
            if s.phase.busy() {
                return false;
            }
            s.generation += 1;
            s.phase = Phase::Downloading {
                done: 0,
                total: release.dmg.as_ref().map_or(0, |d| d.size),
            };
            s.version = Some(release.version.clone());
            s.cancel = Some(Arc::clone(&cancel));
            s.generation
        };
        self.inner.notify();
        DiagnosticsLog::instance().log("update", &format!("start: {}", release.version));
        *self.inner.workers_lock() += 1;
        let inner = Arc::clone(&self.inner);
        let release = release.clone();
        let spawned = std::thread::Builder::new()
            .name("self-update".into())
            .spawn(move || {
                let _done = WorkerDone(&inner);
                inner.attempt(generation, &release, &cancel);
            });
        if let Err(e) = spawned {
            self.inner.worker_ended();
            self.inner.finish(
                generation,
                Phase::Failed(format!("could not start the update: {e}")),
            );
            return false;
        }
        true
    }

    /// Cancels the download under way: the phase is back to idle at once
    /// (from where "Update now" starts afresh), and the transfer stops at its
    /// next read, taking its private directory with it. `false` when no
    /// download is under way: an install is not stopped half way.
    pub fn cancel(&self) -> bool {
        {
            let mut s = self.state();
            if !matches!(s.phase, Phase::Downloading { .. }) {
                return false;
            }
            if let Some(cancel) = s.cancel.take() {
                cancel.store(true, Ordering::SeqCst);
            }
            // The attempt's late reports — a transfer that ended just as
            // Cancel was pressed — land on an attempt that is over.
            s.generation += 1;
            s.phase = Phase::Idle;
        }
        DiagnosticsLog::instance().log("update", "download cancelled");
        self.inner.notify();
        true
    }

    /// Puts a failure away (the banner's dismiss): back to idle.
    pub fn dismiss_failure(&self) {
        {
            let mut s = self.state();
            if !matches!(s.phase, Phase::Failed(_)) {
                return;
            }
            s.phase = Phase::Idle;
        }
        self.inner.notify();
    }

    /// Registers `listener`, called (on whatever thread) after every change
    /// of the phase.
    pub fn add_listener(&self, listener: Box<dyn Fn() + Send + Sync>) -> ListenerId {
        let id = ListenerId(self.inner.next_listener.fetch_add(1, Ordering::Relaxed));
        self.inner
            .listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((id, Arc::from(listener)));
        id
    }

    /// Unregisters a listener.
    pub fn remove_listener(&self, id: ListenerId) {
        self.inner
            .listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(l, _)| *l != id);
    }

    /// Waits until no update thread runs any more — a cancelled one
    /// included, which may still be cleaning up — for at most `timeout`;
    /// whether none does. For tests, and anything that must see the files
    /// settle.
    pub fn wait_for_workers(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut workers = self.inner.workers_lock();
        while *workers > 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            workers = self
                .inner
                .workers_done
                .wait_timeout(workers, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        true
    }
}

/// Counts an update thread out however it ends.
struct WorkerDone<'a>(&'a Inner);

impl Drop for WorkerDone<'_> {
    fn drop(&mut self) {
        self.0.worker_ended();
    }
}

/// How an attempt ended, before its phase is written.
enum Outcome {
    /// Cancelled: the phase moved on already.
    Cancelled,
    /// This copy cannot replace itself, and why.
    Blocked(String),
    Failed(String),
    /// The helper waits for Vocara to exit.
    Installed,
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn workers_lock(&self) -> MutexGuard<'_, usize> {
        self.workers.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn worker_ended(&self) {
        let mut workers = self.workers_lock();
        *workers = workers.saturating_sub(1);
        self.workers_done.notify_all();
    }

    /// Calls every listener, with no lock held (a listener may read the
    /// phase).
    fn notify(&self) {
        let listeners: Vec<Listener> = self
            .listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(_, l)| Arc::clone(l))
            .collect();
        for listener in listeners {
            listener();
        }
    }

    /// Sets the phase of attempt `generation`, unless it is over (cancelled,
    /// superseded): then its result is dropped.
    fn finish(&self, generation: u64, phase: Phase) {
        {
            let mut s = self.state();
            if s.generation != generation {
                return;
            }
            s.cancel = None;
            s.phase = phase;
        }
        self.notify();
    }

    /// Whether attempt `generation` is still the one under way.
    fn current(&self, generation: u64) -> bool {
        self.state().generation == generation
    }

    /// One attempt, on its own thread: see the module docs.
    fn attempt(&self, generation: u64, release: &UpdateInfo, cancel: &AtomicBool) {
        let outcome = self.run(generation, release, cancel);
        let log = DiagnosticsLog::instance();
        match outcome {
            Outcome::Cancelled => {}
            Outcome::Blocked(reason) => {
                log.log("update", "this copy cannot update itself");
                {
                    let mut s = self.state();
                    s.blocker.get_or_insert(Some(reason));
                }
                self.finish(generation, Phase::Idle);
            }
            Outcome::Failed(reason) => {
                log.log("update", "not installed");
                self.finish(generation, Phase::Failed(reason));
            }
            Outcome::Installed => {
                log.log(
                    "update",
                    &format!("staged {}; relaunching", release.version),
                );
                self.finish(generation, Phase::Relaunching);
            }
        }
    }

    fn run(&self, generation: u64, release: &UpdateInfo, cancel: &AtomicBool) -> Outcome {
        // Asked again rather than taken on an earlier answer's word: the
        // install re-checks all of it too, but an attempt that cannot
        // complete must stop before the download, not after it.
        let blocker = self.installer.blocker();
        self.state().blocker = Some(blocker.clone());
        if let Some(reason) = blocker {
            return Outcome::Blocked(reason);
        }
        let Some(dmg) = &release.dmg else {
            return Outcome::Blocked(NO_IMAGE.to_string());
        };
        // The manifest's parser refused any other address already; checked
        // again where it is fetched.
        if !is_release_asset_url(&dmg.url) {
            return Outcome::Failed(format!(
                "a download from outside Vocara's releases was blocked: {}",
                dmg.url
            ));
        }
        let updates = match (self.updates_dir)() {
            Ok(dir) => dir,
            Err(e) => {
                return Outcome::Failed(format!(
                    "no data directory to download the update into ({e})"
                ))
            }
        };
        let dest = match files::create_download_dest(&updates, &release.version) {
            Ok(dest) => dest,
            Err(e) => return Outcome::Failed(e),
        };
        // The first bytes are reported at once (the bar starts moving), the
        // last ones too, and in between at most every PROGRESS_INTERVAL.
        let mut last: Option<Instant> = None;
        let downloaded =
            download::download_to(&*self.transport, &dmg.url, &dest, dmg, cancel, |done| {
                if done == dmg.size || last.is_none_or(|at| at.elapsed() >= PROGRESS_INTERVAL) {
                    last = Some(Instant::now());
                    self.progress(generation, done, dmg.size);
                }
            });
        if let Err(e) = downloaded {
            files::discard_download(&dest);
            if cancel.load(Ordering::SeqCst) || !self.current(generation) {
                return Outcome::Cancelled;
            }
            return Outcome::Failed(e);
        }
        // Cancelled as the last byte landed: nothing will install it.
        if !self.current(generation) {
            files::discard_download(&dest);
            return Outcome::Cancelled;
        }
        {
            let mut s = self.state();
            if s.generation != generation {
                drop(s);
                files::discard_download(&dest);
                return Outcome::Cancelled;
            }
            s.cancel = None;
            s.phase = Phase::Installing;
        }
        self.notify();
        let installed = self.installer.install(&InstallRequest {
            dmg: &dest,
            expected: &release.version,
            running: &self.running,
            updates_dir: &updates,
        });
        // The image has done its job either way: a success copied the
        // verified app out of it, and a retry downloads afresh.
        files::discard_download(&dest);
        match installed {
            Ok(()) => Outcome::Installed,
            Err(e) => Outcome::Failed(e),
        }
    }

    /// The download's progress, for the attempt still under way.
    fn progress(&self, generation: u64, done: u64, total: u64) {
        {
            let mut s = self.state();
            if s.generation != generation || !matches!(s.phase, Phase::Downloading { .. }) {
                return;
            }
            s.phase = Phase::Downloading { done, total };
        }
        self.notify();
    }
}

#[cfg(test)]
mod tests;
