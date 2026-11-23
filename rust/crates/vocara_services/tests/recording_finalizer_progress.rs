//! Port of the quit case of `client/test/asset_import_progress_test.dart`:
//! the real finalizer over the asset store's real import progress. (Its
//! store-level case, and the progress rule played inline, are in
//! `vocara_store`'s `tests/asset_store/asset_import_progress_test.rs`.)
//!
//! A test binary of its own: the import progress reading is process-wide
//! (Dart's statics), so no other import may run while this one is read.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use vocara_services::recording_finalizer::RecordingFinalizer;
use vocara_store::db::durable_fs::DurableFs;
use vocara_store::db::{AppDatabase, AssetStore};
use vocara_store::sync::hlc::HlcClock;
use vocara_store::sync::sync_writer::SyncWriter;

/// Flushes that each take `delay` (F_FULLFSYNC of a large file just copied,
/// on a busy disk), signalling the first.
struct SlowFs {
    delay: Duration,
    first_flush: Mutex<Option<mpsc::Sender<()>>>,
}

impl SlowFs {
    fn flush(&self) {
        if let Some(tx) = self.first_flush.lock().unwrap().take() {
            let _ = tx.send(());
        }
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

#[test]
fn a_quit_waits_for_a_save_still_landing_a_large_file_past_its_budget_instead_of_cutting_it_off() {
    // Each flush is short, but together they outlast one budget window. Two
    // or more land in every window, so only a step stalled for a whole window
    // (not scheduling jitter on a loaded machine) could read as a wedge.
    let tmp = tempfile::tempdir().unwrap();
    // Pre-made, so every flush the slow store sees comes AFTER the copy: the
    // landing tail this file is about.
    std::fs::create_dir_all(tmp.path().join("assets").join(".staging")).unwrap();
    let db = Arc::new(AppDatabase::open_in_memory().unwrap());
    let writer = Arc::new(SyncWriter::new(
        db.clone(),
        Arc::new(HlcClock::new(format!("{:0<26}", "NODE"))),
    ));
    let (first_tx, first_flush) = mpsc::channel();
    let fs = Arc::new(SlowFs {
        delay: Duration::from_millis(300),
        first_flush: Mutex::new(Some(first_tx)),
    });
    let store = Arc::new(AssetStore::new(db, tmp.path(), writer).with_fs(fs));
    let finalizer = Arc::new(
        RecordingFinalizer::for_testing()
            .with_per_finalizer_budget(Duration::from_millis(800))
            .with_total_budget(Duration::from_millis(800))
            .with_progress_ceiling(Duration::from_secs(20))
            .with_progress(AssetStore::bytes_being_copied),
    );
    let src = tmp.path().join("video.mov");
    std::fs::write(&src, vec![7u8; 256 * 1024]).unwrap();
    let src = src.to_string_lossy().into_owned();
    let landed = Arc::new(AtomicBool::new(false));
    let save = {
        let (finalizer, store, landed) = (
            Arc::clone(&finalizer),
            Arc::clone(&store),
            Arc::clone(&landed),
        );
        thread::spawn(move || {
            finalizer.hold(|| {
                store.import_file(&src, AssetStore::DEFAULT_KIND).unwrap();
                landed.store(true, Ordering::SeqCst);
            })
        })
    };

    // Quit once the copy is done and the file is landing.
    first_flush.recv().unwrap();
    assert!(!landed.load(Ordering::SeqCst));
    finalizer.finalize_all();
    let landed_when_quit_returned = landed.load(Ordering::SeqCst);
    save.join().unwrap();

    assert!(
        landed_when_quit_returned,
        "the quit exited while the save was still landing its file"
    );
}
