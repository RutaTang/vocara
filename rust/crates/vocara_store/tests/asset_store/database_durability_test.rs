//! Port of the asset-GC case of `client/test/database_durability_test.dart`
//! (its other two cases cover Persistence and the migrator's connections,
//! not the asset store).

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rusqlite::Connection;
use vocara_store::db::AppDatabase;

use super::{shared, Fixture};

#[test]
fn a_gc_pass_commits_all_of_its_tombstones_together_not_one_drive_flush_per_asset() {
    let _g = shared();
    // Counts what reaches the disk as a commit: every top-level transaction,
    // plus every write statement run outside one (an autocommit) — SQLite's
    // commit hook sees both.
    let commits = Arc::new(AtomicUsize::new(0));
    let mut conn = Connection::open_in_memory().unwrap();
    {
        let commits = commits.clone();
        conn.commit_hook(Some(move || {
            commits.fetch_add(1, Ordering::SeqCst);
            false // let it commit
        }))
        .unwrap();
    }
    let f = Fixture::with_db(AppDatabase::from_connection(conn).unwrap());
    const N: u8 = 6;
    let mut paths = Vec::new();
    for i in 0..N {
        let id = f.import(&f.make_src(&format!("take{i}.wav"), &[i, i + 1, i + 2]));
        paths.push(f.resolve(&id).unwrap());
    }
    // Unreferenced and past the age grace window: all garbage.
    f.db.execute("UPDATE assets SET created_at = 1, updated_at = 1", [])
        .unwrap();

    commits.store(0, Ordering::SeqCst);
    let removed = f.store.sweep_unreferenced(Duration::ZERO).unwrap();

    assert_eq!(removed, usize::from(N));
    for path in &paths {
        assert!(!Path::new(path).exists(), "bytes reclaimed");
    }
    let tombstones =
        f.db.query(
            "SELECT 1 AS x FROM oplog WHERE op = 'tombstone'",
            [],
            |_| Ok(()),
        )
        .unwrap();
    assert_eq!(
        tombstones.len(),
        usize::from(N),
        "every delete still journaled"
    );
    // One for the tombstones, one for the cached peaks of what was
    // reclaimed.
    assert!(commits.load(Ordering::SeqCst) <= 2, "{commits:?}");
}
