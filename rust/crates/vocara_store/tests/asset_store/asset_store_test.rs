//! Port of `client/test/asset_store_test.dart`.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use vocara_store::db::AssetStore;
use vocara_store::Error;

use super::{alone, files_under, pad_id, shared, DirFlushLog, Fixture};

#[test]
fn imports_a_file_into_the_content_addressed_pool_and_journals_it() {
    let _g = shared();
    let f = Fixture::new();
    let src = f.make_src("src.wav", &[1, 2, 3, 4, 5]);
    let id = f.import(&src);

    assert_eq!(id.len(), 26);
    let row = f.asset(&id);
    assert_eq!(row.byte_size, 5);
    assert_eq!(row.content_hash.len(), 64); // sha256 hex
    assert!(row
        .local_rel_path
        .as_deref()
        .unwrap()
        .starts_with("assets/"));
    assert_eq!(row.bytes_sync_state, "localOnly");

    let abs = f.resolve(&id).expect("resolves");
    assert_eq!(fs::read(&abs).unwrap(), [1, 2, 3, 4, 5]);
    // The file lives under the sharded pool, not at the source path.
    assert!(Path::new(&abs).starts_with(f.root().join("assets")));
    assert_ne!(abs, src);

    let inserts = f
        .oplog()
        .into_iter()
        .filter(|o| o.entity_table == "assets" && o.op == "insert")
        .count();
    assert_eq!(inserts, 1);
}

#[test]
fn dedups_identical_bytes_to_one_asset_and_one_file() {
    let _g = shared();
    let f = Fixture::new();
    let a = f.make_src("a.wav", &[9, 9, 9]);
    let b = f.make_src("b.wav", &[9, 9, 9]); // same bytes, different source
    let id1 = f.import(&a);
    let id2 = f.import(&b);

    assert_eq!(id1, id2);
    assert_eq!(f.assets().len(), 1);
}

// The copy was promoted before the dedup lookup, and the orphan sweep keeps
// every file of a live hash: it stayed for the asset's whole life.
#[test]
fn the_same_bytes_under_a_second_extension_keep_one_pool_file() {
    let _g = shared();
    let f = Fixture::new();
    let a = f.make_src("a.wav", &[3; 1000]);
    let b = f.make_src("b.m4a", &[3; 1000]);
    let id1 = f.import(&a);
    let id2 = f.import(&b);
    assert_eq!(id2, id1);

    let pooled: Vec<String> = files_under(&f.root().join("assets"))
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .filter(|n| !n.ends_with(".part"))
        .collect();
    assert_eq!(pooled.len(), 1, "{pooled:?}");
    assert!(pooled[0].ends_with(".wav"), "the row's file stays");
    assert!(Path::new(&f.resolve(&id1).unwrap()).exists());
}

#[test]
fn another_name_of_a_rows_own_file_is_not_trashed_as_a_duplicate() {
    let _g = shared();
    // On a case-insensitive volume (macOS's default) `<hash>.WAV` is the
    // row's `<hash>.wav`: importing the same bytes as x.WAV reused that file,
    // found no row on the upper-case path, and moved the row's only copy to
    // the trash. Where names are case-sensitive, a hard link stands in.
    let f = Fixture::new();
    let a = f.make_src("a.wav", &[3; 1000]);
    let b = f.make_src("b.WAV", &[3; 1000]);
    let id = f.import(&a);
    let own = PathBuf::from(f.resolve(&id).unwrap());
    let stem = own.file_stem().unwrap().to_string_lossy().into_owned();
    let alias = own.with_file_name(format!("{stem}.WAV"));
    if !alias.exists() {
        fs::hard_link(&own, &alias).unwrap();
    }

    assert_eq!(f.import(&b), id);
    assert!(own.exists(), "the row lost its bytes");
    assert!(alias.exists(), "moved to the trash");
}

#[test]
fn a_copy_another_import_claims_meanwhile_is_not_trashed_under_it() {
    let _g = shared();
    // Imports share the write gate. This one deduped its .m4a copy to a .wav
    // row and was about to move the copy to the trash; meanwhile that row was
    // deleted, and another import of the same bytes as .m4a promoted onto the
    // copy and went on to point a new row at it. The move left that asset
    // with no bytes.
    let f = Fixture::new();
    let a = f.make_src("a.wav", &[3; 1000]);
    let b = f.make_src("b.m4a", &[3; 1000]);
    let c = f.make_src("c.m4a", &[3; 1000]);
    let id1 = f.import(&a);

    let (promoted_tx, promoted_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let promoted_tx = Arc::new(Mutex::new(Some(promoted_tx)));
    let promoted_rx = Arc::new(Mutex::new(promoted_rx));
    let release_rx = Arc::new(Mutex::new(release_rx));
    let other: Arc<Mutex<Option<thread::JoinHandle<String>>>> = Arc::new(Mutex::new(None));

    let weak = Arc::downgrade(&f.store);
    let db = f.db.clone();
    let hook_id1 = id1.clone();
    let hook_other = other.clone();
    f.store
        .set_debug_before_trashing_duplicate(Some(Arc::new(move || {
            let store = weak.upgrade().unwrap();
            store.set_debug_before_trashing_duplicate(None);
            db.update_by_id("assets", &hook_id1, &[("deleted_at", &1i64)])
                .unwrap();
            let promote_weak = Arc::downgrade(&store);
            let promoted_tx = promoted_tx.clone();
            let release_rx = release_rx.clone();
            store.set_debug_after_promote(Some(Arc::new(move |_rel: &str| {
                if let Some(store) = promote_weak.upgrade() {
                    store.set_debug_after_promote(None);
                }
                if let Some(tx) = promoted_tx.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                let _ = release_rx.lock().unwrap().recv();
            })));
            let importer = store.clone();
            let c = c.clone();
            *hook_other.lock().unwrap() = Some(thread::spawn(move || {
                importer.import_file(&c, AssetStore::DEFAULT_KIND).unwrap()
            }));
            // Let it promote, where its turn allows.
            let _ = promoted_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_millis(500));
        })));

    assert_eq!(f.import(&b), id1);
    release_tx.send(()).unwrap();
    let claimed = other.lock().unwrap().take().unwrap().join().unwrap();
    assert_ne!(claimed, id1);
    let file = f
        .resolve(&claimed)
        .expect("the claiming asset lost its bytes");
    assert!(Path::new(&file).exists());
}

#[test]
fn different_bytes_produce_different_assets() {
    let _g = shared();
    let f = Fixture::new();
    let id1 = f.import(&f.make_src("a.wav", &[1]));
    let id2 = f.import(&f.make_src("b.wav", &[2]));
    assert_ne!(id1, id2);
    assert_eq!(f.assets().len(), 2);
}

#[test]
fn resolve_re_materializes_a_deduped_asset_whose_file_went_missing() {
    let _g = shared();
    let f = Fixture::new();
    let src = f.make_src("src.wav", &[7, 7, 7, 7]);
    let id = f.import(&src);
    let abs = f.resolve(&id).unwrap();
    fs::remove_file(&abs).unwrap(); // simulate local GC of the bytes
    assert_eq!(f.resolve(&id), None);
    // Re-importing the same bytes reuses the row AND re-places the file.
    let id2 = f.import(&f.make_src("again.wav", &[7, 7, 7, 7]));
    assert_eq!(id2, id);
    assert!(f.resolve(&id).is_some());
}

#[test]
fn import_tolerates_two_live_assets_sharing_a_content_hash() {
    let _g = shared();
    // The cross-device steady state, instead of throwing.
    let f = Fixture::new();
    let id_a = f.import(&f.make_src("dup.wav", &[3, 1, 4, 1, 5]));
    let hash = f.asset(&id_a).content_hash;
    // Simulate a synced remote asset: identical bytes under a different id
    // (the schema keeps content_hash NON-unique on purpose).
    let id_b = "BBBBBBBBBBBBBBBBBBBBBBBBBB";
    f.db.execute(
        "INSERT INTO assets (id, content_hash, byte_size, created_at, updated_at, \
         hlc_wall, hlc_ctr, hlc_node, hlc_packed) \
         VALUES (?, ?, 5, 1, 1, 1, 0, 'NNNNNNNNNNNNNNNNNNNNNNNNNN', 'z')",
        [id_b, hash.as_str()],
    )
    .unwrap();

    // Two live rows now share the hash — the import must resolve, not throw.
    let again = f.import(&f.make_src("dup2.wav", &[3, 1, 4, 1, 5]));
    assert!(again == id_a || again == id_b, "{again}");
}

#[test]
fn six_concurrent_imports_of_the_same_bytes_all_succeed_and_dedupe_to_one_asset() {
    let _g = shared();
    // The old shared .part name tore each other apart.
    let f = Fixture::new();
    let sources: Vec<String> = (0..6)
        // Big enough that the streaming copies genuinely interleave.
        .map(|i| f.make_src(&format!("c{i}.wav"), &vec![7u8; 256 * 1024]))
        .collect();
    let handles: Vec<_> = sources
        .into_iter()
        .map(|s| {
            let store = f.store.clone();
            thread::spawn(move || store.import_file(&s, AssetStore::DEFAULT_KIND))
        })
        .collect();
    let ids: HashSet<String> = handles
        .into_iter()
        .map(|h| h.join().unwrap().unwrap())
        .collect();
    assert_eq!(ids.len(), 1);
    assert_eq!(f.assets().len(), 1);
    let abs = f.resolve(ids.iter().next().unwrap()).expect("resolves");
    assert_eq!(fs::metadata(&abs).unwrap().len(), 256 * 1024);
    // No staging leftovers: every .part was promoted or cleaned up.
    let staging = f.root().join("assets").join(".staging");
    if staging.exists() {
        assert_eq!(fs::read_dir(&staging).unwrap().count(), 0);
    }
}

#[test]
fn the_first_import_into_an_empty_pool_flushes_every_directory_it_creates() {
    let _g = shared();
    // Into its parent, not just the file into its shard.
    let f = Fixture::new();
    let log = Arc::new(DirFlushLog::default());
    let logged = f.store_with(log.clone());
    let id = logged
        .import_file(
            &f.make_src("take.wav", &[1, 2, 3]),
            AssetStore::DEFAULT_KIND,
        )
        .unwrap();
    let rel = f.asset(&id).local_rel_path.unwrap();

    // assets/<ab>/<cd>/<hash>.wav: this import created assets/, <ab> and
    // <cd> as well as the file, so a power cut after the row commits must
    // find every link of that chain — each new directory flushed into its
    // parent (root, assets/, <ab>), not only the file into <cd>.
    let shard = Path::new(&rel).parent().unwrap().to_path_buf();
    let flushed: Vec<PathBuf> = log
        .dirs
        .lock()
        .unwrap()
        .iter()
        .map(|d| {
            let r = d.strip_prefix(f.root()).unwrap().to_path_buf();
            if r.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                r
            }
        })
        .collect();
    for want in [
        PathBuf::from("."),
        PathBuf::from("assets"),
        shard.parent().unwrap().to_path_buf(),
        shard.clone(),
    ] {
        assert!(flushed.contains(&want), "{want:?} not in {flushed:?}");
    }
}

#[test]
fn a_truncated_pool_file_left_by_a_crash_is_replaced_not_trusted() {
    let _g = shared();
    let f = Fixture::new();
    let src = f.make_src("src.wav", &[1, 2, 3, 4, 5, 6, 7, 8]);
    let id = f.import(&src);
    let abs = f.resolve(&id).unwrap();
    // Simulate a crash leftover: the pool path exists but holds fewer bytes.
    fs::write(&abs, [1, 2, 3]).unwrap();
    let id2 = f.import(&src);
    assert_eq!(id2, id);
    assert_eq!(
        fs::metadata(&abs).unwrap().len(),
        8,
        "existence alone must not win"
    );
}

#[test]
fn resolve_or_import_of_a_pool_path_a_live_asset_owns_returns_that_asset_without_staging() {
    let _g = shared();
    // Without staging a copy or re-hashing the bytes.
    let f = Fixture::new();
    let id = f.import(&f.make_src("take.wav", &[1, 2, 3, 4]));
    // What hydration hands every clip, and so what every later save passes.
    let pooled = f.resolve(&id).unwrap();
    let ops_before = f.oplog().len();

    // Bytes changed under the content address after pooling (rot / an
    // outside edit — the case verify_pool_integrity reports and leaves
    // alone)...
    fs::write(&pooled, [1, 2, 3, 5]).unwrap();
    // ...and no room to stage anything: a file where the staging dir goes.
    let staging = f.root().join("assets").join(".staging");
    if staging.exists() {
        fs::remove_dir_all(&staging).unwrap();
    }
    fs::write(&staging, [0]).unwrap();
    // (A genuine import really is blocked now — the case below isn't
    // vacuous.)
    let blocked = f
        .store
        .import_file(&f.make_src("other.wav", &[9]), AssetStore::DEFAULT_KIND);
    assert!(matches!(blocked, Err(Error::Io(_))), "{blocked:?}");

    assert_eq!(
        f.store
            .resolve_or_import(&pooled, AssetStore::DEFAULT_KIND)
            .unwrap(),
        id
    );
    // No new asset minted for the changed bytes, nothing journaled, and the
    // pooled file is still exactly what was there.
    assert_eq!(f.assets().len(), 1);
    assert_eq!(f.oplog().len(), ops_before);
    assert_eq!(fs::read(&pooled).unwrap(), [1, 2, 3, 5]);
}

#[test]
fn resolve_or_import_still_imports_and_dedups_a_file_that_is_not_a_live_assets_pool_path() {
    let _g = shared();
    let f = Fixture::new();
    let id = f.import(&f.make_src("take.wav", &[6, 6, 6]));
    // An external copy of the same bytes dedups to the existing asset...
    assert_eq!(
        f.store
            .resolve_or_import(
                &f.make_src("copy.wav", &[6, 6, 6]),
                AssetStore::DEFAULT_KIND
            )
            .unwrap(),
        id
    );
    // ...and new bytes, even placed inside the pool tree, are imported.
    let stray = f.make_src("assets/stray.wav", &[8, 8]);
    let stray_id = f
        .store
        .resolve_or_import(&stray, AssetStore::DEFAULT_KIND)
        .unwrap();
    assert_ne!(stray_id, id);
    assert_eq!(f.assets().len(), 2);
    assert_eq!(fs::read(f.resolve(&stray_id).unwrap()).unwrap(), [8, 8]);
}

#[test]
fn resolve_or_import_all_answers_each_reference_exactly_as_resolve_or_import_would() {
    let _g = shared();
    // At that point of a sequential walk.
    let f = Fixture::new();
    let a = f.import(&f.make_src("a.wav", &[1, 1]));
    let pooled_a = f.resolve(&a).unwrap();
    // A pool file whose only asset row is tombstoned has no live owner, so
    // its first reference IMPORTS it — minting the live owner that a later
    // reference to the same path must then resolve to (an owner map read
    // once up front would miss it and import a second time).
    let t = f.import(&f.make_src("t.wav", &[2, 2]));
    let pooled_t = f.resolve(&t).unwrap();
    f.tombstone(&t);
    let fresh = f.make_src("fresh.wav", &[3, 3]);
    let missing = pad_id("M");

    let paths = vec![
        pooled_a.clone(),
        AssetStore::ref_for(&missing),
        fresh.clone(),
        pooled_t.clone(),
        pooled_t.clone(),
        fresh.clone(),
        pooled_a.clone(),
    ];
    let ids = f
        .store
        .resolve_or_import_all(&paths, AssetStore::DEFAULT_KIND)
        .unwrap();

    assert_eq!(ids[0], a);
    assert_eq!(ids[1], missing); // an asset:// ref keeps its embedded id
    assert!(ids[2] != a && ids[2] != t); // imported
    assert!(ids[3] != a && ids[3] != t && ids[3] != ids[2]); // re-imported, new live row
    assert_eq!(ids[4], ids[3]); // the owner that import just minted
    assert_eq!(ids[5], ids[2]); // deduped by content
    assert_eq!(ids[6], a);
    assert_eq!(f.assets().len(), 4);
    // One at a time, on the store this left, every answer is the same.
    for (i, path) in paths.iter().enumerate() {
        assert_eq!(
            f.store
                .resolve_or_import(path, AssetStore::DEFAULT_KIND)
                .unwrap(),
            ids[i],
            "#{i}"
        );
    }
}

#[test]
fn sweep_staging_clears_audio_and_technique_media_but_never_the_pool() {
    let _g = shared();
    let f = Fixture::new();
    // A pooled asset (under assets/) plus stray staging files.
    let id = f.import(&f.make_src("keep.wav", &[4, 2]));
    let pooled = f.resolve(&id).unwrap();

    let staged = [
        f.make_src("audio/p1/take.wav", &[0]),
        f.make_src("technique_media/t1/ex.wav", &[0]),
    ];

    f.store.sweep_staging(&HashSet::new(), &HashSet::new());

    for s in &staged {
        assert!(!Path::new(s).exists()); // staging reclaimed
    }
    assert!(!f.root().join("audio").exists());
    assert!(Path::new(&pooled).exists()); // pool untouched
    assert!(f.resolve(&id).is_some());
}

#[test]
fn a_kept_staging_file_keeps_its_sidecars_and_only_those() {
    let _g = shared();
    let f = Fixture::new();
    let kept = f.make_src("audio/p1/take.wav", &[0]);
    let sidecars = [
        f.make_src("audio/p1/take.wav.sha256", &[0]),
        f.make_src("audio/p1/take.wav.retry", &[0]),
    ];
    // A dot in a folder name is not a sidecar's: nothing in it is kept.
    let in_folder = f.make_src("audio/p1/take.wav.d/other", &[0]);
    let other = f.make_src("audio/p1/other.wav", &[0]);

    f.store
        .sweep_staging(&HashSet::from([kept.clone()]), &HashSet::new());

    assert!(Path::new(&kept).exists());
    for s in &sidecars {
        assert!(Path::new(s).exists(), "{s}");
    }
    assert!(!Path::new(&in_folder).exists());
    assert!(!Path::new(&other).exists());
}

#[test]
fn a_landed_import_leaves_nothing_counted_as_still_copying() {
    let _g = alone();
    // A quit waits on a Save that is still pooling a large file, judged by
    // this count moving (see RecordingFinalizer): a staging file left in it
    // would read as a copy in progress for the rest of the session.
    assert_eq!(AssetStore::copies_in_flight(), 0);
    let f = Fixture::new();
    f.import(&f.make_src("big.wav", &vec![7u8; 512 * 1024]));
    // The set itself, not the byte count: a path left in it names a file
    // already renamed into the pool, which reads as 0 bytes either way.
    assert_eq!(AssetStore::copies_in_flight(), 0);
}

#[test]
fn a_copy_in_flight_is_counted_while_its_bytes_land() {
    let _g = alone();
    // The quit's only evidence that a Save past its budget is still working.
    let f = Fixture::new();
    let src = f.make_src("huge.wav", &vec![7u8; 32 << 20]);
    let baseline = AssetStore::bytes_being_copied();
    let mut copying = false;
    let mut seen = baseline;
    let store = f.store.clone();
    let import = thread::spawn(move || store.import_file(&src, AssetStore::DEFAULT_KIND));
    while !copying || seen == baseline {
        if import.is_finished() {
            break;
        }
        copying |= AssetStore::copies_in_flight() == 1;
        seen = seen.max(AssetStore::bytes_being_copied());
        thread::sleep(Duration::from_millis(1));
    }
    import.join().unwrap().unwrap();
    assert!(copying, "the copy was never listed");
    assert!(seen > baseline, "its bytes were never counted");
    assert_eq!(AssetStore::copies_in_flight(), 0);
}

// A staged recording remembers the hash its import measured. Boot recovery
// judges each staged take by its content hash; a graceful stop leaves the
// session's takes in staging after the save pooled them, and the next boot
// re-read every one in full to hash it again.

fn staged(f: &Fixture, name: &str, bytes: &[u8]) -> PathBuf {
    PathBuf::from(f.make_src(&format!("audio/practice1/{name}"), bytes))
}

fn ramp(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i & 0xff) as u8).collect()
}

#[test]
fn a_staged_recording_remembers_its_hash_and_reports_it_while_the_file_is_unchanged() {
    let _g = shared();
    let f = Fixture::new();
    let file = staged(&f, "take.wav", &ramp(4096));
    f.import(&file.to_string_lossy());
    assert_eq!(
        AssetStore::known_hash_of(&file),
        Some(AssetStore::sha256_of_file(&file).unwrap())
    );
    fs::OpenOptions::new()
        .append(true)
        .open(&file)
        .unwrap()
        .write_all(&[1, 2, 3])
        .unwrap();
    assert_eq!(AssetStore::known_hash_of(&file), None, "it changed");
}

#[test]
fn a_staged_recording_remembers_its_hash_only_for_files_in_the_staging_tree() {
    let _g = shared();
    let f = Fixture::new();
    let elsewhere = PathBuf::from(f.make_src("import.wav", &[7; 64]));
    f.import(&elsewhere.to_string_lossy());
    assert_eq!(AssetStore::known_hash_of(&elsewhere), None);
    assert!(!f.root().join("import.wav.sha256").exists());
}

#[test]
fn a_staged_recording_remembers_its_hash_only_for_recordings_boot_recovery_looks_at() {
    let _g = shared();
    // Not the imports and edits staged below them.
    let f = Fixture::new();
    let below = staged(&f, "imports/guide.wav", &[9; 64]);
    f.import(&below.to_string_lossy());
    let sidecar = format!("{}.sha256", below.to_string_lossy());
    assert!(!Path::new(&sidecar).exists());
    assert_eq!(AssetStore::imported_hash_of(&below), None);
}

#[test]
fn a_staged_recording_lands_its_hash_on_the_drive_before_the_import_goes_on() {
    let _g = shared();
    // Boot recovery trusts this record over a staging file a power cut
    // rolled back, so it must be on disk before any row names the bytes.
    let f = Fixture::new();
    let log = Arc::new(DirFlushLog::default());
    let logged = f.store_with(log.clone());
    let file = staged(&f, "take.wav", &ramp(512));
    logged
        .import_file(&file.to_string_lossy(), AssetStore::DEFAULT_KIND)
        .unwrap();
    assert!(log
        .dirs
        .lock()
        .unwrap()
        .contains(&file.parent().unwrap().to_path_buf()));
    // Kept whatever later happens to the file: it answers for what the
    // import pooled.
    fs::write(&file, [1, 2, 3]).unwrap();
    assert_eq!(AssetStore::known_hash_of(&file), None);
    let imported = AssetStore::imported_hash_of(&file);
    assert!(imported.is_some());
    assert_ne!(imported, Some(AssetStore::sha256_of_file(&file).unwrap()));
}
