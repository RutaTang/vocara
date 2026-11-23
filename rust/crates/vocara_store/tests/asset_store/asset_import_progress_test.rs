//! Port of `client/test/asset_import_progress_test.dart`.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use vocara_store::db::durable_fs::DurableFs;
use vocara_store::db::AssetStore;

use super::{alone, Fixture};

/// Flushes that each take `delay` (F_FULLFSYNC of a large file just copied,
/// on a busy disk), recording what the import reported while each ran.
struct SlowFs {
    delay: Duration,
    first_flush: Mutex<Option<mpsc::Sender<()>>>,
    readings: Mutex<Vec<i64>>,
    in_flight: Mutex<Vec<usize>>,
}

impl SlowFs {
    fn new(delay: Duration) -> (Arc<Self>, mpsc::Receiver<()>) {
        let (tx, rx) = mpsc::channel();
        (
            Arc::new(SlowFs {
                delay,
                first_flush: Mutex::new(Some(tx)),
                readings: Mutex::new(Vec::new()),
                in_flight: Mutex::new(Vec::new()),
            }),
            rx,
        )
    }

    fn flush(&self) {
        if let Some(tx) = self.first_flush.lock().unwrap().take() {
            let _ = tx.send(());
        }
        self.readings
            .lock()
            .unwrap()
            .push(AssetStore::bytes_being_copied());
        self.in_flight
            .lock()
            .unwrap()
            .push(AssetStore::copies_in_flight());
        thread::sleep(self.delay);
    }
}

impl DurableFs for SlowFs {
    fn fsync_file(&self, _path: &Path) -> io::Result<()> {
        self.flush();
        Ok(())
    }

    fn fsync_dir(&self, _path: &Path) {
        self.flush();
    }
}

/// A library whose staging directory is pre-made, so every flush the slow
/// store sees comes AFTER the copy: the landing tail this file is about.
fn fixture() -> Fixture {
    let f = Fixture::new();
    std::fs::create_dir_all(f.root().join("assets").join(".staging")).unwrap();
    f
}

fn source(f: &Fixture) -> String {
    f.make_src("video.mov", &vec![7u8; 256 * 1024])
}

#[test]
fn an_import_stays_listed_and_its_progress_keeps_moving_while_the_file_lands() {
    let _g = alone();
    // While the copied file is flushed, placed and committed.
    let f = fixture();
    let (fs, _first) = SlowFs::new(Duration::from_millis(20));
    let store = f.store_with(fs.clone());
    store
        .import_file(&source(&f), AssetStore::DEFAULT_KIND)
        .unwrap();

    // Shard dirs, the file, its directory entry: four flushes after the copy.
    let readings = fs.readings.lock().unwrap().clone();
    assert_eq!(readings.len(), 4);
    assert!(
        fs.in_flight.lock().unwrap().iter().all(|&n| n == 1),
        "the import was dropped from the list once its copy finished"
    );
    for pair in readings.windows(2) {
        assert!(
            pair[1] > pair[0],
            "the reading stood still across a finished landing step: {readings:?}"
        );
    }
    let after = AssetStore::bytes_being_copied();
    assert!(after > readings[3], "the commit counts");
    assert_eq!(AssetStore::copies_in_flight(), 0);
}

#[test]
fn a_quit_waits_for_a_save_still_landing_a_large_file_past_its_budget() {
    let _g = alone();
    // Instead of cutting it off. Each flush is short, but together they
    // outlast one budget window. Two or more land in every window, so only a
    // step stalled for a whole window (not scheduling jitter on a loaded
    // machine) could read as a wedge.
    //
    // The quit's progress rule is played here as RecordingFinalizer runs it —
    // wait one budget window; past it, keep waiting only while the reading
    // moved since the last look, up to the ceiling. The same case against the
    // real finalizer is vocara_services' tests/recording_finalizer_progress.rs.
    let f = fixture();
    let (fs, first_flush) = SlowFs::new(Duration::from_millis(300));
    let store = Arc::new(f.store_with(fs));
    let src = source(&f);
    let landed = Arc::new(AtomicBool::new(false));
    let save = {
        let (store, landed) = (store.clone(), landed.clone());
        thread::spawn(move || {
            store.import_file(&src, AssetStore::DEFAULT_KIND).unwrap();
            landed.store(true, Ordering::SeqCst);
        })
    };

    // Quit once the copy is done and the file is landing.
    first_flush.recv().unwrap();
    assert!(!landed.load(Ordering::SeqCst));
    let budget = Duration::from_millis(800);
    let ceiling = Duration::from_secs(20);
    let quit = Instant::now();
    let mut mark = AssetStore::bytes_being_copied();
    loop {
        let window = Instant::now();
        while window.elapsed() < budget && !landed.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(5));
        }
        if landed.load(Ordering::SeqCst) {
            break;
        }
        let now = AssetStore::bytes_being_copied();
        if now == mark || quit.elapsed() >= ceiling {
            break; // a wedge: the quit goes on without the save
        }
        mark = now;
    }
    let landed_when_quit_returned = landed.load(Ordering::SeqCst);
    save.join().unwrap();

    assert!(
        landed_when_quit_returned,
        "the quit exited while the save was still landing its file"
    );
}
