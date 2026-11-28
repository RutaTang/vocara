//! A self-update for the app's tests: a release image served from memory, a
//! held transfer the test lets through read by read, and an installer that
//! records what it was handed instead of swapping anything. No network, no
//! disk image.

use std::io::{self, Read};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use vocara_services::self_update::download::sha256;
use vocara_services::self_update::{InstallRequest, Installer, Phase, SelfUpdate, Transport};
use vocara_services::update_check_service::{DmgAsset, UpdateInfo};

/// Where the fakes' release image is.
pub(crate) const IMAGE_URL: &str =
    "https://github.com/RutaTang/vocara/releases/download/v9.9.9/Vocara-9.9.9.dmg";

/// The release image the fakes serve: 2500 bytes, so three reads.
pub(crate) fn image() -> Vec<u8> {
    vec![7u8; 2500]
}

/// A manifest advertising `version` with `notes`, naming `image` when
/// given (the shape release.yml writes), else in the shape before the
/// self-update.
pub(crate) fn manifest(version: &str, notes: &str, image: Option<&[u8]>) -> String {
    let mut body = serde_json::json!({
        "latest": version,
        "url": format!("https://github.com/RutaTang/vocara/releases/tag/v{version}"),
        "notes": notes,
    });
    if let Some(image) = image {
        let asset = asset(image);
        body["dmg"] = serde_json::json!({
            "url": asset.url,
            "size": asset.size,
            "sha256": asset.sha256_hex(),
        });
    }
    body.to_string()
}

/// The image entry of a manifest for `image`.
fn asset(image: &[u8]) -> DmgAsset {
    DmgAsset {
        url: IMAGE_URL.into(),
        size: image.len() as u64,
        sha256: sha256(image),
    }
}

/// The release 9.9.9, naming `image` when given.
pub(crate) fn release(image: Option<&[u8]>) -> UpdateInfo {
    UpdateInfo {
        version: "9.9.9".into(),
        url: "https://github.com/RutaTang/vocara/releases/tag/v9.9.9".into(),
        notes: "New voiceprint views.".into(),
        dmg: image.map(asset),
    }
}

/// What the fake installer answers, and what it was handed.
#[derive(Default)]
pub(crate) struct FakeInstaller {
    /// Why this copy cannot replace itself, if it cannot.
    pub(crate) blocker: Mutex<Option<String>>,
    /// Why the install fails, if it does.
    pub(crate) fails_with: Mutex<Option<String>>,
    /// How many images it installed.
    pub(crate) installs: AtomicUsize,
    /// When set, the next install waits here, Installing, until a send (or
    /// the sender's drop) lets it go on.
    pub(crate) held: Mutex<Option<mpsc::Receiver<()>>>,
}

impl Installer for FakeInstaller {
    fn blocker(&self) -> Option<String> {
        self.blocker.lock().unwrap().clone()
    }

    fn install(&self, request: &InstallRequest<'_>) -> Result<(), String> {
        if let Some(e) = self.fails_with.lock().unwrap().clone() {
            return Err(e);
        }
        // Verifying and staging, which take the Mac's installer seconds.
        let held = self.held.lock().unwrap().take();
        if let Some(held) = held {
            let _ = held.recv();
        }
        // As the Mac's installer does, right before it starts the helper.
        if !(request.clear_to_swap)() {
            return Err("the update was cancelled before the swap".into());
        }
        self.installs.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// Serves the image, a read at a time once let through when held.
struct Served {
    image: Vec<u8>,
    gate: Option<Arc<Mutex<mpsc::Receiver<()>>>>,
}

struct Body {
    image: Vec<u8>,
    at: usize,
    gate: Option<Arc<Mutex<mpsc::Receiver<()>>>>,
}

impl Read for Body {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if let Some(gate) = &self.gate {
            if gate.lock().unwrap().recv().is_err() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "stalled"));
            }
        }
        let n = buf.len().min(1000).min(self.image.len() - self.at);
        buf[..n].copy_from_slice(&self.image[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

impl Transport for Served {
    fn open(&self, url: &str) -> Result<Box<dyn Read + Send>, String> {
        assert_eq!(url, IMAGE_URL, "only the release's image is fetched");
        Ok(Box::new(Body {
            image: self.image.clone(),
            at: 0,
            gate: self.gate.clone(),
        }))
    }
}

/// A self-update over the fakes, and its controls.
pub(crate) struct Updates {
    pub(crate) updater: Arc<SelfUpdate>,
    pub(crate) installer: Arc<FakeInstaller>,
    /// Lets one read of a held transfer through.
    pub(crate) open: Option<mpsc::Sender<()>>,
    _dir: tempfile::TempDir,
}

/// A self-update that can replace "this copy" (once [`SelfUpdate::prepare`]
/// asked), serving [`image`]; its transfer held read by read when `held`.
pub(crate) fn updates(held: bool) -> Updates {
    updates_waiting_on(held, Arc::new(std::sync::atomic::AtomicBool::new(false)))
}

/// [`updates`], its install waiting while `take` is set (a take being
/// recorded or saved, or other work the relaunch would cut short).
pub(crate) fn updates_waiting_on(held: bool, take: Arc<std::sync::atomic::AtomicBool>) -> Updates {
    let dir = tempfile::tempdir().unwrap();
    let updates_dir = dir.path().join("updates");
    let (open, gate) = if held {
        let (open, gate) = mpsc::channel();
        (Some(open), Some(Arc::new(Mutex::new(gate))))
    } else {
        (None, None)
    };
    let installer = Arc::new(FakeInstaller::default());
    let updater = Arc::new(
        SelfUpdate::new(
            Box::new(move || Ok(updates_dir.clone())),
            Arc::new(Served {
                image: image(),
                gate,
            }),
            Arc::clone(&installer) as Arc<dyn Installer>,
            "1.0.0",
        )
        .waiting_on_work(move || take.load(std::sync::atomic::Ordering::SeqCst)),
    );
    Updates {
        updater,
        installer,
        open,
        _dir: dir,
    }
}

impl Updates {
    /// Lets `reads` reads of the held transfer through.
    pub(crate) fn let_through(&self, reads: usize) {
        let open = self.open.as_ref().expect("a held transfer");
        for _ in 0..reads {
            let _ = open.send(());
        }
    }

    /// Waits until the phase is `wanted`.
    pub(crate) fn wait_for(&self, wanted: &Phase) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.updater.phase() != *wanted {
            assert!(
                Instant::now() < deadline,
                "the update never got to {wanted:?}: {:?}",
                self.updater.phase()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Waits until the update's thread has ended.
    pub(crate) fn settle(&self) {
        assert!(
            self.updater.wait_for_workers(Duration::from_secs(10)),
            "the update thread never ended"
        );
    }
}

impl Drop for Updates {
    fn drop(&mut self) {
        // A held transfer still waiting gives up, and its thread ends.
        self.open = None;
        let _ = self.updater.wait_for_workers(Duration::from_secs(10));
    }
}
