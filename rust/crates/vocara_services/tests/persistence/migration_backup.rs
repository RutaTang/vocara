//! Port of `client/test/persistence_migration_backup_test.dart`.
//!
//! The pre-migration rollback copy (vocara.pre-migrate.bak) is the way back
//! while a freshly migrated database is unproven, and a whole extra copy of
//! the library once it is proven. It goes on the first boot whose
//! readability probe and staged-take recovery succeed, and stays on any boot
//! that fails either.
//!
//! The file's third case (the backup is never discarded while a migration
//! marker is pending) is the migrator's own and is ported with it
//! (`safe_migrator`'s
//! `the_backup_is_never_discarded_while_a_migration_marker_is_pending`).

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use vocara_store::db::safe_migrator::SafeMigrator;
use vocara_store::db::CURRENT_SCHEMA_VERSION;
use vocara_store::repositories::SongPracticeRepository;

use crate::support::*;

fn db_path(root: &Path) -> PathBuf {
    root.join("vocara.db")
}

fn backup(root: &Path) -> PathBuf {
    root.join(SafeMigrator::BAK_NAME)
}

/// A library one schema version behind, with one practice in it: built at
/// the current version, then stripped back to v1 (v2 only added the three
/// local-only sync tables).
fn seed_v1_library(root: &Path) -> String {
    let p = boot(root);
    let id = create_practice(&p).id;
    p.close().unwrap();
    let raw = Connection::open(db_path(root)).unwrap();
    raw.execute_batch(
        "DROP TABLE sync_state; DROP TABLE sync_quarantine; DROP TABLE asset_sync; \
         PRAGMA user_version = 1;",
    )
    .unwrap();
    id
}

#[test]
fn a_boot_that_migrated_the_library_and_proved_it_readable_discards_the_backup() {
    let root = tempfile::tempdir().unwrap();
    let practice_id = seed_v1_library(root.path());

    let p = boot(root.path());

    // The migration ran and the data came through.
    let check = Connection::open_with_flags(db_path(root.path()), OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap();
    let version: i64 = check
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, CURRENT_SCHEMA_VERSION);
    drop(check);
    assert!(p.song_repo().get_practice(&practice_id).is_ok());
    assert!(
        !backup(root.path()).exists(),
        "a whole second copy of the library was left behind for good"
    );
}

// The probe and staged-take recovery open one gate together (a failure of
// either closes it), so a failed probe stands for both here.
#[test]
fn a_boot_whose_readability_probe_fails_keeps_the_backup() {
    let root = tempfile::tempdir().unwrap();
    seed_v1_library(root.path());
    // Corrupt a table the probe reads; the migration itself still succeeds.
    Connection::open(db_path(root.path()))
        .unwrap()
        .execute_batch("DROP TABLE clips")
        .unwrap();

    let _p = boot(root.path());

    assert!(backup(root.path()).exists());
}
