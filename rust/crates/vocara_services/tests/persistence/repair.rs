//! Port of `client/test/repair_library_saves_test.dart`, and the
//! service-level assertion of `settings_screen_test.dart`'s Repair-library
//! flow (a clean store: nothing found, nothing swept). The rest of that
//! widget test — the confirm dialog, its copy, the report toast — is the
//! Settings screen's.

use std::fs;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use vocara_services::persistence::RepairReport;
use vocara_store::repositories::SongPracticeRepository;

use crate::support::*;

#[test]
fn a_save_made_while_repair_library_re_hashes_the_pool_does_not_wait_for_the_re_hash() {
    // The re-hash only reads, yet it ran inside the write gate's exclusive
    // side: every save — an edit, a technique, the save that arms REC — sat
    // behind a pass over the whole pool, minutes long for a large library.
    let m = Memory::new();
    m.song_repo().create_practice("Song").unwrap();
    // A pooled file a few long takes in size, for the re-hash to chew on.
    let hash = "ab".repeat(32);
    let shard = m.root.join("assets").join("ab").join("ab");
    fs::create_dir_all(&shard).unwrap();
    let mut raf = fs::File::create(shard.join(format!("{hash}.wav"))).unwrap();
    let mut chunk = vec![0u8; 1 << 20];
    for i in 0..128u8 {
        chunk[0] = i;
        raf.write_all(&chunk).unwrap();
    }
    drop(raf);

    let repaired = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let repair = scope.spawn(|| {
            let report = m.repair_library();
            repaired.store(true, Ordering::SeqCst);
            report
        });
        // Past the integrity pass and the sweeps: the re-hash is under way.
        std::thread::sleep(Duration::from_millis(200));
        let saved = m.song_repo().create_practice("Edited meanwhile");
        let repaired_before_the_save = repaired.load(Ordering::SeqCst);
        let report = repair.join().unwrap().unwrap();

        assert!(saved.is_ok(), "{saved:?}");
        assert!(
            !repaired_before_the_save,
            "the save waited for the whole re-hash"
        );
        assert_eq!(report.corrupt_pool_files, 1, "the re-hash still reports");
    });
}

#[test]
fn repair_on_a_clean_store_finds_and_sweeps_nothing() {
    let m = Memory::new();
    let report = m.repair_library().unwrap();
    assert_eq!(report, RepairReport::default());
    assert_eq!(
        report.to_string(),
        "RepairReport(found: 0, repaired: 0, assetsSwept: 0, corruptPoolFiles: 0)"
    );
}
