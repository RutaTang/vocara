//! Ports of the TechniqueMediaService parts of
//! `client/test/import_cap_test.dart` and
//! `client/test/technique_media_pool_guard_test.dart`.
//!
//! The pool guard's view-model cases (a dispose-time cleanup failure
//! contained, Cancel and Delete surviving a failed delete, a refused import's
//! notice, the end-to-end removed-paths flow) are left to the technique view
//! models' ports; the guard they lean on — `delete_media` refusing pool files
//! — is pinned here.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use vocara_services::audio_file_service::{AudioFileError, MAX_IMPORT_BYTES};
use vocara_services::local_storage_service::LocalStorageService;
use vocara_services::technique_media_service::TechniqueMediaService;
use vocara_store::db::{AppDatabase, AssetStore};
use vocara_store::sync::hlc::HlcClock;
use vocara_store::sync::sync_writer::SyncWriter;

const TECHNIQUE: &str = "T1000000000000000000000000";

/// A media service rooted at [root] (the SAME root the asset store uses, so
/// the pool-root guard is exercised).
fn media(root: &Path) -> TechniqueMediaService {
    TechniqueMediaService::new(Arc::new(LocalStorageService::with_root(root)))
}

fn make_src(root: &Path, name: &str, bytes: &[u8]) -> String {
    let path = root.join(name);
    fs::write(&path, bytes).unwrap();
    path.to_string_lossy().into_owned()
}

fn format_message<T: std::fmt::Debug>(r: Result<T, AudioFileError>) -> String {
    match r {
        Err(AudioFileError::Format(m)) => m,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// A sparse file whose LENGTH exceeds the import cap without the disk cost.
fn oversized(dir: &Path) -> String {
    let f = dir.join("huge.wav");
    let file = fs::File::create(&f).unwrap();
    file.set_len(MAX_IMPORT_BYTES + 1).unwrap();
    f.to_string_lossy().into_owned()
}

// --- import_cap_test.dart --------------------------------------------------

#[test]
fn import_media_refuses_an_empty_source() {
    // Staged and saved, it was an example that could neither play nor be
    // analyzed, where the song screen refuses the same file.
    let tmp = tempfile::tempdir().unwrap();
    let src = make_src(tmp.path(), "empty.mp3", &[]);
    assert_eq!(
        format_message(media(tmp.path()).import_media(TECHNIQUE, &src)),
        "This file is empty."
    );
    assert!(!tmp.path().join("technique_media").exists());
}

#[test]
fn import_media_refuses_an_oversized_source_the_same_way() {
    let tmp = tempfile::tempdir().unwrap();
    let src = oversized(tmp.path());
    let m = format_message(media(tmp.path()).import_media(TECHNIQUE, &src));
    assert!(m.contains("too large"), "{m}");
    assert!(!tmp.path().join("technique_media").exists());
}

#[test]
fn a_normal_sized_source_imports_under_the_technique_keeping_its_extension() {
    let tmp = tempfile::tempdir().unwrap();
    let src = make_src(tmp.path(), "clip.m4a", &[1, 2, 3, 4]);
    let dest = media(tmp.path()).import_media(TECHNIQUE, &src).unwrap();
    assert_eq!(fs::read(&dest).unwrap(), [1, 2, 3, 4]);
    assert!(dest.ends_with(".m4a"), "{dest}");
    assert_eq!(
        Path::new(&dest).parent().unwrap(),
        tmp.path().join("technique_media").join(TECHNIQUE)
    );
    // The source stays where the user keeps it.
    assert!(Path::new(&src).exists());
}

#[test]
fn a_missing_source_or_a_folder_fails_before_any_folder_is_made() {
    let tmp = tempfile::tempdir().unwrap();
    let service = media(tmp.path());
    let missing = tmp.path().join("gone.wav").to_string_lossy().into_owned();
    assert!(matches!(
        service.import_media(TECHNIQUE, &missing),
        Err(AudioFileError::Io(_))
    ));
    let folder = tmp.path().join("folder.wav");
    fs::create_dir(&folder).unwrap();
    assert!(matches!(
        service.import_media(TECHNIQUE, &folder.to_string_lossy()),
        Err(AudioFileError::Io(_))
    ));
    assert!(!tmp.path().join("technique_media").exists());
}

// --- technique_media_pool_guard_test.dart -----------------------------------

#[test]
fn delete_media_refuses_pool_files_and_asset_refs_still_deletes_staging_copies() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Arc::new(AppDatabase::open_in_memory().unwrap());
    let writer = Arc::new(SyncWriter::new(
        db.clone(),
        Arc::new(HlcClock::new(format!("{:0<26}", "NODE"))),
    ));
    let assets = AssetStore::new(db, tmp.path(), writer);
    let service = media(tmp.path());
    let id = assets
        .import_file(
            &make_src(tmp.path(), "a.wav", &[1, 2, 3]),
            AssetStore::DEFAULT_KIND,
        )
        .unwrap();
    let pooled = assets.resolve(&id).unwrap().unwrap();

    // Content-addressed pool bytes are owned by the hash-refcount GC.
    service.delete_media(&pooled).unwrap();
    assert!(Path::new(&pooled).exists());

    // A bytes-missing-locally reference is not a path at all.
    service.delete_media(&AssetStore::ref_for(&id)).unwrap(); // must not fail

    // A draft staging copy (what the service itself imports) is still deleted.
    let staged = service
        .import_media(TECHNIQUE, &make_src(tmp.path(), "b.wav", &[4, 5]))
        .unwrap();
    assert!(Path::new(&staged).exists());
    service.delete_media(&staged).unwrap();
    assert!(!Path::new(&staged).exists());
}

#[test]
fn a_path_metacharacter_technique_id_never_reaches_the_filesystem() {
    // Ids become directory names and recursive-delete targets. The sync wire
    // door quarantines hostile ids; this is the service-level last line.
    let tmp = tempfile::tempdir().unwrap();
    let service = media(tmp.path());
    for evil in ["../..", "a/b", "..", "AAAAAAAAAAAAAAAAAAAAAAAA/."] {
        assert!(
            matches!(
                service.import_media(evil, "irrelevant"),
                Err(AudioFileError::InvalidId(_))
            ),
            "{evil}"
        );
        assert!(
            matches!(
                service.delete_technique_media(evil),
                Err(AudioFileError::InvalidId(_))
            ),
            "{evil}"
        );
    }
    // A well-formed id still works end to end (import_media exercised above).
    service.delete_technique_media(TECHNIQUE).unwrap();
}

#[test]
fn delete_media_with_the_storage_root_torn_down_completes_silently_and_creates_nothing() {
    // The dispose path fires deletes off while the app (or a test teardown)
    // is removing the storage root. The delete must neither fail nor
    // recreate any directory: the old shape resolved the pool guard via
    // asset_dir, whose mkdir raced the teardown into a not-found error.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    fs::create_dir(&root).unwrap();
    let service = media(&root);
    let staged = service
        .import_media(TECHNIQUE, &make_src(tmp.path(), "gone.wav", &[7, 8]))
        .unwrap();
    fs::remove_dir_all(&root).unwrap();

    service.delete_media(&staged).unwrap(); // must not fail
    service.delete_technique_media(TECHNIQUE).unwrap(); // must not fail

    assert!(
        !root.exists(),
        "a delete must never mkdir the storage tree back into existence"
    );
}

#[test]
fn delete_technique_media_removes_the_whole_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let service = media(tmp.path());
    let a = service
        .import_media(TECHNIQUE, &make_src(tmp.path(), "a.wav", &[1]))
        .unwrap();
    let b = service
        .import_media(TECHNIQUE, &make_src(tmp.path(), "b.mp4", &[2]))
        .unwrap();
    assert_ne!(a, b);
    service.delete_technique_media(TECHNIQUE).unwrap();
    assert!(!tmp.path().join("technique_media").join(TECHNIQUE).exists());
    assert!(tmp.path().join("technique_media").exists());
}
