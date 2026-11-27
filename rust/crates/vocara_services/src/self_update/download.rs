//! The update image's download: HTTPS only, capped, cancellable, its
//! progress reported, its size and SHA-256 checked against the manifest's
//! (Clew's `update::download_to`, with the manifest's checksum on top).
//!
//! The transfer is a [`Transport`] — [`HttpsTransport`] in the app, an
//! in-memory one in the tests, which contact no network — and everything
//! around it, from the file it lands in to the checks on what arrived, is
//! [`download_to`] and [`stream_verified`], the same code either way.
//!
//! What a match proves is integrity: these are the bytes the manifest
//! names, whole. It does not make them trustworthy — the manifest is a file
//! on a web server — which is the installer's job (`super::install`).

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::MAX_UPDATE_BYTES;
use crate::update_check_service::DmgAsset;

/// How a download reaches its bytes.
pub trait Transport: Send + Sync {
    /// Requests `url` and hands back its body to read. Blocking.
    fn open(&self, url: &str) -> Result<Box<dyn Read + Send>, String>;
}

/// The app's transport: `ureq` over HTTPS, redirects included (GitHub sends
/// a release asset's download on to its storage host), with timeouts that
/// stop a stalled transfer, and no transparent compression (the bytes
/// counted and hashed are the bytes of the file).
#[derive(Debug, Default, Clone, Copy)]
pub struct HttpsTransport;

/// How long the connection may take to open.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// How long one read may wait for bytes: a transfer that goes quiet for
/// this long has stalled. It is also how long a cancelled download can take
/// to notice, at worst — the app has moved on at once either way.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// The whole transfer's bound. Slow links are fine (every read has its own
/// stall timeout); this only stops one that trickles forever.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);

impl Transport for HttpsTransport {
    fn open(&self, url: &str) -> Result<Box<dyn Read + Send>, String> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            // Every hop, redirects included, over https.
            .https_only(true)
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_per_read(Some(READ_TIMEOUT))
            .timeout_global(Some(TRANSFER_TIMEOUT))
            .build()
            .into();
        let response = agent
            .get(url)
            .header("User-Agent", "vocara-updater")
            .header("Accept", "application/octet-stream")
            // The file's own bytes: what the manifest's size and digest are
            // of.
            .header("Accept-Encoding", "identity")
            .call()
            .map_err(|e| format!("download failed: {e}"))?;
        if response.status() != 200 {
            return Err(format!("download failed: status {}", response.status()));
        }
        // One byte past the cap is enough to tell an image too large:
        // `stream_verified` stops at the manifest's size anyway.
        let body = response
            .into_body()
            .into_with_config()
            .limit(MAX_UPDATE_BYTES + 1)
            .reader();
        Ok(Box::new(body))
    }
}

/// Downloads `asset` from `url` into `dest`, a new file (see
/// [`create_new`]), reporting the bytes so far as they arrive, and checks
/// that exactly `asset.size` bytes with digest `asset.sha256` arrived.
/// HTTPS only, refused before anything is opened; abandoned at the next read
/// once `cancel` is set. A failure leaves `dest` for the caller to discard
/// (its directory is the attempt's own). Blocking.
pub fn download_to(
    transport: &dyn Transport,
    url: &str,
    dest: &Path,
    asset: &DmgAsset,
    cancel: &AtomicBool,
    progress: impl FnMut(u64),
) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err(format!("only HTTPS downloads are allowed, not {url}"));
    }
    if asset.size == 0 || asset.size > MAX_UPDATE_BYTES {
        return Err(format!(
            "the release names an image of {} bytes, which Vocara does not download",
            asset.size
        ));
    }
    let mut file = create_new(dest).map_err(|e| format!("could not save the download: {e}"))?;
    if cancel.load(Ordering::SeqCst) {
        return Err(CANCELLED.to_string());
    }
    let mut body = transport.open(url)?;
    stream_verified(
        &mut body,
        &mut file,
        asset.size,
        &asset.sha256,
        cancel,
        progress,
    )?;
    file.sync_all()
        .map_err(|e| format!("could not save the download: {e}"))
}

/// What a cancelled download fails with.
pub const CANCELLED: &str = "the download was cancelled";

/// The SHA-256 of `bytes`, as a manifest names an image's (for a test's
/// manifest, and anything that must name one).
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// How much is read at a time.
const CHUNK: usize = 64 * 1024;

/// Copies `body` into `out`, at most `size` bytes, hashing them as they
/// pass, reporting the count after each read, and checking on the way out
/// that exactly `size` bytes with SHA-256 `sha256` came: more is refused as
/// soon as the first extra byte arrives (nothing past the cap is written),
/// fewer — a connection that dropped mid-image — is a failure, not a short
/// image handed on, and a wrong digest is refused. `cancel` is looked at
/// before every read.
pub fn stream_verified(
    body: &mut dyn Read,
    out: &mut dyn Write,
    size: u64,
    sha256: &[u8; 32],
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64),
) -> Result<(), String> {
    let mut hasher = Sha256::new();
    let mut done: u64 = 0;
    let mut buf = vec![0u8; CHUNK];
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(CANCELLED.to_string());
        }
        let n = match body.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("download failed: {e}")),
        };
        if done + n as u64 > size {
            return Err(format!(
                "the download is larger than the {size} bytes the release names"
            ));
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n])
            .map_err(|e| format!("could not save the download: {e}"))?;
        done += n as u64;
        progress(done);
    }
    if cancel.load(Ordering::SeqCst) {
        return Err(CANCELLED.to_string());
    }
    if done < size {
        return Err(format!(
            "the download ended after {done} of its {size} bytes"
        ));
    }
    if hasher.finalize().as_slice() != sha256 {
        return Err("the download does not match the release's checksum".to_string());
    }
    Ok(())
}

/// Opens `path` for writing as a NEW file, private to this user (0600), and
/// never through a symlink: an existing entry — a planted link included —
/// fails the download instead of redirecting it. (The directory it lands in
/// is the attempt's own and fresh, so nothing should be there.)
pub fn create_new(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn sha(bytes: &[u8]) -> [u8; 32] {
        Sha256::digest(bytes).into()
    }

    fn never() -> AtomicBool {
        AtomicBool::new(false)
    }

    /// A body of `bytes` that hands out at most `step` bytes a read.
    struct Trickle {
        bytes: Vec<u8>,
        at: usize,
        step: usize,
    }

    impl Read for Trickle {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.step.min(buf.len()).min(self.bytes.len() - self.at);
            buf[..n].copy_from_slice(&self.bytes[self.at..self.at + n]);
            self.at += n;
            Ok(n)
        }
    }

    fn trickle(bytes: &[u8], step: usize) -> Trickle {
        Trickle {
            bytes: bytes.to_vec(),
            at: 0,
            step,
        }
    }

    #[test]
    fn exactly_the_named_bytes_pass_and_are_reported_as_they_come() {
        let image = vec![7u8; 10_000];
        let mut out = Vec::new();
        let mut seen = Vec::new();
        stream_verified(
            &mut trickle(&image, 4096),
            &mut out,
            image.len() as u64,
            &sha(&image),
            &never(),
            |done| seen.push(done),
        )
        .unwrap();
        assert_eq!(out, image);
        assert_eq!(seen, [4096, 8192, 10_000]);
    }

    /// More than the manifest names is refused at the first extra byte, and
    /// nothing past the cap is written.
    #[test]
    fn a_larger_download_is_refused_before_its_extra_bytes_are_written() {
        let image = vec![1u8; 5000];
        let mut out = Vec::new();
        let err = stream_verified(
            &mut trickle(&image, 1000),
            &mut out,
            4500,
            &sha(&image[..4500]),
            &never(),
            |_| {},
        )
        .unwrap_err();
        assert!(err.contains("larger than the 4500 bytes"), "{err}");
        assert_eq!(out.len(), 4000, "nothing past the cap");
    }

    /// A connection that drops mid-image is a failed download, not a short
    /// image handed on to the installer.
    #[test]
    fn a_truncated_download_is_a_failure() {
        let image = vec![2u8; 100];
        let err = stream_verified(
            &mut trickle(&image, 64),
            &mut Vec::new(),
            4096,
            &sha(&image),
            &never(),
            |_| {},
        )
        .unwrap_err();
        assert_eq!(err, "the download ended after 100 of its 4096 bytes");
    }

    #[test]
    fn a_download_whose_checksum_differs_is_refused() {
        let image = b"the real image".to_vec();
        let mut forged = image.clone();
        forged[0] ^= 1;
        let err = stream_verified(
            &mut trickle(&forged, 4),
            &mut Vec::new(),
            image.len() as u64,
            &sha(&image),
            &never(),
            |_| {},
        )
        .unwrap_err();
        assert!(err.contains("checksum"), "{err}");
    }

    /// Cancel stops the transfer at its next read.
    #[test]
    fn a_cancelled_download_stops_at_its_next_read() {
        let image = vec![3u8; 1_000_000];
        let cancel = AtomicBool::new(false);
        let mut out = Vec::new();
        let err = stream_verified(
            &mut trickle(&image, 1000),
            &mut out,
            image.len() as u64,
            &sha(&image),
            &cancel,
            |done| {
                if done >= 3000 {
                    cancel.store(true, Ordering::SeqCst);
                }
            },
        )
        .unwrap_err();
        assert_eq!(err, CANCELLED);
        assert_eq!(out.len(), 3000);
    }

    /// A read that fails is the download's failure.
    #[test]
    fn a_failing_read_fails_the_download() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::ConnectionReset, "reset"))
            }
        }
        let err = stream_verified(&mut Broken, &mut Vec::new(), 1, &[0; 32], &never(), |_| {})
            .unwrap_err();
        assert!(err.starts_with("download failed"), "{err}");
    }

    /// A transport that serves `body` and records what it was asked for.
    struct Served {
        body: Vec<u8>,
        asked: Mutex<Vec<String>>,
    }

    impl Transport for Served {
        fn open(&self, url: &str) -> Result<Box<dyn Read + Send>, String> {
            self.asked.lock().unwrap().push(url.to_string());
            Ok(Box::new(io::Cursor::new(self.body.clone())))
        }
    }

    fn asset(bytes: &[u8]) -> DmgAsset {
        DmgAsset {
            url: "https://github.com/RutaTang/vocara/releases/download/v9.9.9/Vocara-9.9.9.dmg"
                .into(),
            size: bytes.len() as u64,
            sha256: sha(bytes),
        }
    }

    /// The whole download: the file is new, private, and holds the image.
    #[test]
    fn a_download_lands_in_a_new_private_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let image = vec![9u8; 70_000];
        let served = Served {
            body: image.clone(),
            asked: Mutex::new(Vec::new()),
        };
        let a = asset(&image);
        let dest = dir.path().join("Vocara-9.9.9.dmg");
        download_to(&served, &a.url, &dest, &a, &never(), |_| {}).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), image);
        let mode = std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(*served.asked.lock().unwrap(), [a.url.as_str()]);

        // Never over an existing entry, nor through a planted link.
        let err = download_to(&served, &a.url, &dest, &a, &never(), |_| {}).unwrap_err();
        assert!(err.contains("could not save"), "{err}");
        let link = dir.path().join("link.dmg");
        std::os::unix::fs::symlink(dir.path().join("elsewhere"), &link).unwrap();
        assert!(download_to(&served, &a.url, &link, &a, &never(), |_| {}).is_err());
        assert!(!dir.path().join("elsewhere").exists());
    }

    /// A plain-http address is refused before the transport is asked.
    #[test]
    fn downloads_are_https_only() {
        let dir = tempfile::tempdir().unwrap();
        let served = Served {
            body: Vec::new(),
            asked: Mutex::new(Vec::new()),
        };
        let mut a = asset(b"x");
        a.url = "http://github.com/RutaTang/vocara/releases/download/v1/Vocara.dmg".into();
        let err = download_to(
            &served,
            &a.url,
            &dir.path().join("a.dmg"),
            &a,
            &never(),
            |_| {},
        )
        .unwrap_err();
        assert!(err.contains("only HTTPS"), "{err}");
        assert!(served.asked.lock().unwrap().is_empty());
        assert!(!dir.path().join("a.dmg").exists());
    }

    /// An image larger than the cap is not even requested.
    #[test]
    fn an_image_over_the_cap_is_not_requested() {
        let dir = tempfile::tempdir().unwrap();
        let served = Served {
            body: Vec::new(),
            asked: Mutex::new(Vec::new()),
        };
        let mut a = asset(b"x");
        a.size = MAX_UPDATE_BYTES + 1;
        assert!(download_to(
            &served,
            &a.url.clone(),
            &dir.path().join("a.dmg"),
            &a,
            &never(),
            |_| {}
        )
        .is_err());
        assert!(served.asked.lock().unwrap().is_empty());
    }
}
