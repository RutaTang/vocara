//! The self-update's state machine over a fake transport and installer: no
//! network, no disk image, no swap — what the service decides, and what it
//! leaves on disk.

use std::io::{self, Read};
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc;

use sha2::{Digest, Sha256};

use super::*;
use crate::update_check_service::DmgAsset;

const URL: &str = "https://github.com/RutaTang/vocara/releases/download/v9.9.9/Vocara-9.9.9.dmg";

fn release(image: &[u8]) -> UpdateInfo {
    UpdateInfo {
        version: "9.9.9".into(),
        url: "https://github.com/RutaTang/vocara/releases/tag/v9.9.9".into(),
        notes: "New voiceprint views.".into(),
        dmg: Some(DmgAsset {
            url: URL.into(),
            size: image.len() as u64,
            sha256: Sha256::digest(image).into(),
        }),
    }
}

/// Serves an image, each read held until the test lets it through when
/// `gated`.
struct FakeTransport {
    image: Vec<u8>,
    /// Each read waits for one of these first, when given.
    gate: Option<Mutex<mpsc::Receiver<()>>>,
    asked: AtomicUsize,
}

impl FakeTransport {
    fn serving(image: &[u8]) -> Arc<FakeTransport> {
        Arc::new(FakeTransport {
            image: image.to_vec(),
            gate: None,
            asked: AtomicUsize::new(0),
        })
    }

    fn gated(image: &[u8]) -> (Arc<FakeTransport>, mpsc::Sender<()>) {
        let (open, gate) = mpsc::channel();
        let transport = Arc::new(FakeTransport {
            image: image.to_vec(),
            gate: Some(Mutex::new(gate)),
            asked: AtomicUsize::new(0),
        });
        (transport, open)
    }
}

struct GatedBody {
    transport: Arc<FakeTransport>,
    at: usize,
}

impl Read for GatedBody {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if let Some(gate) = &self.transport.gate {
            if gate.lock().unwrap().recv().is_err() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "stalled"));
            }
        }
        let image = &self.transport.image;
        let n = buf.len().min(1000).min(image.len() - self.at);
        buf[..n].copy_from_slice(&image[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

/// A transport handed to the service, sharing the fake's state.
struct Shared(Arc<FakeTransport>);

impl Transport for Shared {
    fn open(&self, url: &str) -> Result<Box<dyn Read + Send>, String> {
        assert_eq!(url, URL);
        self.0.asked.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(GatedBody {
            transport: Arc::clone(&self.0),
            at: 0,
        }))
    }
}

/// What the fake installer was asked, and how it answers.
#[derive(Default)]
struct FakeInstaller {
    blocker: Mutex<Option<String>>,
    fails_with: Mutex<Option<String>>,
    /// The images it was handed: their bytes, the version expected and the
    /// running one.
    installed: Mutex<Vec<(Vec<u8>, String, String)>>,
    blocker_asked: AtomicUsize,
}

impl Installer for FakeInstaller {
    fn blocker(&self) -> Option<String> {
        self.blocker_asked.fetch_add(1, Ordering::SeqCst);
        self.blocker.lock().unwrap().clone()
    }

    fn install(&self, request: &InstallRequest<'_>) -> Result<(), String> {
        assert!(request.dmg.starts_with(request.updates_dir));
        self.installed.lock().unwrap().push((
            std::fs::read(request.dmg).unwrap(),
            request.expected.to_string(),
            request.running.to_string(),
        ));
        match self.fails_with.lock().unwrap().clone() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

struct Fixture {
    updates: tempfile::TempDir,
    installer: Arc<FakeInstaller>,
    service: SelfUpdate,
    changes: Arc<AtomicUsize>,
}

fn fixture(transport: Arc<FakeTransport>) -> Fixture {
    let updates = tempfile::tempdir().unwrap();
    let dir = updates.path().join("updates");
    let installer = Arc::new(FakeInstaller::default());
    let service = SelfUpdate::new(
        Box::new(move || Ok(dir.clone())),
        Arc::new(Shared(transport)),
        Arc::clone(&installer) as Arc<dyn Installer>,
        "0.3.0",
    );
    let changes = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&changes);
    service.add_listener(Box::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
    }));
    Fixture {
        updates,
        installer,
        service,
        changes,
    }
}

impl Fixture {
    fn settle(&self) {
        assert!(
            self.service.wait_for_workers(Duration::from_secs(10)),
            "the update thread never ended"
        );
    }

    /// What is left under the updates directory.
    fn left(&self) -> Vec<String> {
        std::fs::read_dir(self.updates.path().join("updates"))
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The whole way: downloaded, its checksum matched, handed to the installer
/// with the version offered and the running one, and then the app is to
/// quit; the download's directory is gone.
#[test]
fn an_update_downloads_installs_and_asks_the_app_to_quit() {
    let image = vec![7u8; 4321];
    let fx = fixture(FakeTransport::serving(&image));
    assert_eq!(fx.service.phase(), Phase::Idle);
    assert!(fx.service.start(&release(&image)));
    fx.settle();
    assert_eq!(fx.service.phase(), Phase::Relaunching);
    assert!(fx.service.wants_quit());
    assert_eq!(fx.service.version().as_deref(), Some("9.9.9"));
    let installed = fx.installer.installed.lock().unwrap();
    assert_eq!(*installed, [(image, "9.9.9".into(), "0.3.0".into())]);
    assert!(fx.left().is_empty(), "{:?}", fx.left());
    assert!(
        fx.changes.load(Ordering::SeqCst) >= 3,
        "start, installing, relaunching"
    );
    // Under way: nothing starts a second one.
    assert!(!fx.service.start(&release(b"other")));
}

/// Progress is reported as the bytes arrive, of the size the manifest names.
#[test]
fn the_download_reports_its_progress() {
    let image = vec![1u8; 3000];
    let (transport, open) = FakeTransport::gated(&image);
    let fx = fixture(transport);
    assert!(fx.service.start(&release(&image)));
    assert_eq!(
        fx.service.phase(),
        Phase::Downloading {
            done: 0,
            total: 3000
        }
    );
    // Starting it again while it runs does nothing.
    assert!(!fx.service.start(&release(&image)));
    for _ in 0..4 {
        open.send(()).unwrap();
    }
    fx.settle();
    assert_eq!(fx.service.phase(), Phase::Relaunching);
}

/// Cancel stops the download: the phase is back at once, the transfer stops
/// at its next read and takes its directory with it, and nothing is
/// installed — however late its result lands.
#[test]
fn cancel_stops_the_download_and_leaves_nothing() {
    let image = vec![2u8; 5000];
    let (transport, open) = FakeTransport::gated(&image);
    let fx = fixture(Arc::clone(&transport));
    assert!(fx.service.start(&release(&image)));
    open.send(()).unwrap();
    // Wait for the first chunk to land in its directory.
    let deadline = Instant::now() + Duration::from_secs(10);
    while fx.left().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(fx.left().len(), 1, "the attempt's own directory");
    assert!(fx.service.cancel());
    assert_eq!(fx.service.phase(), Phase::Idle, "at once");
    // The rest of the bytes arrive; nobody installs them.
    for _ in 0..10 {
        let _ = open.send(());
    }
    fx.settle();
    assert_eq!(fx.service.phase(), Phase::Idle);
    assert!(fx.installer.installed.lock().unwrap().is_empty());
    assert!(fx.left().is_empty(), "{:?}", fx.left());
    // Nothing to cancel now; "Update now" starts afresh.
    assert!(!fx.service.cancel());
    drop(open);
    let fresh = fixture(FakeTransport::serving(&image));
    assert!(fresh.service.start(&release(&image)));
    fresh.settle();
    assert_eq!(fresh.service.phase(), Phase::Relaunching);
}

/// A stalled transfer that is cancelled ends when its read gives up, and
/// its late failure is not reported over the offer.
#[test]
fn a_cancelled_stalled_download_reports_nothing() {
    let image = vec![3u8; 5000];
    let (transport, open) = FakeTransport::gated(&image);
    let fx = fixture(transport);
    assert!(fx.service.start(&release(&image)));
    assert!(fx.service.cancel());
    drop(open); // the read fails, as a stalled one times out
    fx.settle();
    assert_eq!(fx.service.phase(), Phase::Idle);
    assert!(fx.left().is_empty(), "{:?}", fx.left());
}

/// An image that does not match the manifest's checksum is thrown away, and
/// never reaches the installer.
#[test]
fn an_image_that_does_not_match_its_checksum_is_never_installed() {
    let image = vec![4u8; 2000];
    let mut forged = image.clone();
    forged[1999] ^= 0xff;
    let fx = fixture(FakeTransport::serving(&forged));
    assert!(fx.service.start(&release(&image)));
    fx.settle();
    assert_eq!(
        fx.service.phase(),
        Phase::Failed("the download does not match the release's checksum".into())
    );
    assert!(fx.installer.installed.lock().unwrap().is_empty());
    assert!(fx.left().is_empty(), "{:?}", fx.left());
    // Try again starts afresh; dismissing puts the failure away.
    fx.service.dismiss_failure();
    assert_eq!(fx.service.phase(), Phase::Idle);
}

/// An install that fails says why, and takes the image with it.
#[test]
fn a_failed_install_says_why() {
    let image = vec![5u8; 100];
    let fx = fixture(FakeTransport::serving(&image));
    *fx.installer.fails_with.lock().unwrap() =
        Some("the update's signature is invalid: forged".into());
    assert!(fx.service.start(&release(&image)));
    fx.settle();
    assert_eq!(
        fx.service.phase(),
        Phase::Failed("the update's signature is invalid: forged".into())
    );
    assert!(!fx.service.wants_quit());
    assert!(fx.left().is_empty(), "{:?}", fx.left());
}

/// A copy that cannot replace itself says why BEFORE downloading anything,
/// and the offer turns to the release page.
#[test]
fn a_copy_that_cannot_replace_itself_downloads_nothing() {
    let image = vec![6u8; 100];
    let transport = FakeTransport::serving(&image);
    let fx = fixture(Arc::clone(&transport));
    *fx.installer.blocker.lock().unwrap() =
        Some("Vocara is not running from an installed app".into());
    let offer = release(&image);
    assert_eq!(fx.service.self_install(&offer), SelfInstall::Unknown);
    assert!(fx.service.start(&offer));
    fx.settle();
    assert_eq!(fx.service.phase(), Phase::Idle);
    assert_eq!(
        transport.asked.load(Ordering::SeqCst),
        0,
        "nothing downloaded"
    );
    assert_eq!(
        fx.service.self_install(&offer),
        SelfInstall::Blocked("Vocara is not running from an installed app".into())
    );
    assert!(fx.left().is_empty());
}

/// Whether this copy can replace itself is found out once, off the UI
/// thread, so the offer can say so before anyone clicks.
#[test]
fn prepare_finds_out_once_whether_this_copy_can_replace_itself() {
    let image = vec![6u8; 100];
    let fx = fixture(FakeTransport::serving(&image));
    let offer = release(&image);
    fx.service.prepare();
    fx.service.prepare();
    assert_eq!(fx.installer.blocker_asked.load(Ordering::SeqCst), 1);
    assert_eq!(fx.service.self_install(&offer), SelfInstall::Ready);
    // A release with no usable image: the release page, whatever this copy.
    let mut pageless = offer.clone();
    pageless.dmg = None;
    assert_eq!(
        fx.service.self_install(&pageless),
        SelfInstall::Blocked(NO_IMAGE.into())
    );
}

/// An image address off this repository's releases is never fetched, even
/// were a manifest's parser to let one through.
#[test]
fn an_image_off_the_release_host_is_never_fetched() {
    let image = vec![8u8; 100];
    let transport = FakeTransport::serving(&image);
    let fx = fixture(Arc::clone(&transport));
    let mut offer = release(&image);
    if let Some(dmg) = &mut offer.dmg {
        dmg.url = "https://evil.example/Vocara.dmg".into();
    }
    assert!(fx.service.start(&offer));
    fx.settle();
    assert!(
        matches!(fx.service.phase(), Phase::Failed(e) if e.contains("outside Vocara's releases"))
    );
    assert_eq!(transport.asked.load(Ordering::SeqCst), 0);
}

/// A swap that failed after the last run quit is reported once the
/// relaunched Vocara starts — once — and stale downloads are swept.
#[test]
fn startup_reports_a_failed_swap_once() {
    let fx = fixture(FakeTransport::serving(b""));
    let updates = fx.updates.path().join("updates");
    std::fs::create_dir_all(&updates).unwrap();
    std::fs::write(
        updates.join(files::INSTALL_FAILED_MARKER),
        "could not copy the new version into place; the previous version was restored\n",
    )
    .unwrap();
    fx.service.startup();
    assert_eq!(
        fx.service.phase(),
        Phase::Failed(
            "could not copy the new version into place; the previous version was restored".into()
        )
    );
    assert_eq!(fx.service.version(), None);
    fx.service.dismiss_failure();
    fx.service.startup();
    assert_eq!(fx.service.phase(), Phase::Idle, "once per run");
    assert!(!updates.join(files::INSTALL_FAILED_MARKER).exists());
}

#[test]
fn a_removed_listener_hears_nothing_more() {
    let fx = fixture(FakeTransport::serving(b""));
    let heard = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&heard);
    let id = fx.service.add_listener(Box::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
    }));
    fx.service.prepare();
    assert_eq!(heard.load(Ordering::SeqCst), 1);
    fx.service.remove_listener(id);
    fx.service.dismiss_failure();
    let image = vec![0u8; 10];
    fx.service.start(&release(&image));
    fx.settle();
    assert_eq!(heard.load(Ordering::SeqCst), 1);
}

/// With no data directory there is nowhere private to download to: the
/// update fails rather than falling back to the shared temp directory.
#[test]
fn no_data_directory_fails_the_update_instead_of_using_temp() {
    let installer = Arc::new(FakeInstaller::default());
    let service = SelfUpdate::new(
        Box::new(|| Err(io::Error::other("no home"))),
        Arc::new(Shared(FakeTransport::serving(b"x"))),
        installer,
        "0.3.0",
    );
    assert!(service.start(&release(b"x")));
    assert!(service.wait_for_workers(Duration::from_secs(10)));
    assert!(
        matches!(service.phase(), Phase::Failed(e) if e.contains("no data directory")),
        "{:?}",
        service.phase()
    );
}

/// Off macOS the app cannot update itself, and says so.
#[cfg(not(target_os = "macos"))]
#[test]
fn off_macos_the_platform_installer_says_why() {
    assert_eq!(
        platform_installer().blocker().as_deref(),
        Some("Vocara can update itself only on macOS")
    );
}

/// The app's tests' service: never updates, and says why.
#[test]
fn an_unavailable_self_update_says_why() {
    let service = SelfUpdate::unavailable("not here");
    let offer = release(b"x");
    service.prepare();
    assert_eq!(
        service.self_install(&offer),
        SelfInstall::Blocked("not here".into())
    );
    service.startup();
    assert_eq!(service.phase(), Phase::Idle);
}
