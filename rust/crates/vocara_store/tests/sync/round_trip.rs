//! Port of `client/test/sync_round_trip_test.dart`: two devices sharing ONE
//! fake cloud — the end-to-end convergence suite for P0 sync:
//! create/edit/delete on one device, push/pull, and prove identical row
//! state on the other.
//!
//! The Dart suite drives two full Persistence stacks through the
//! repository; the devices here make the same row-level writes the
//! repository makes (see `device.rs`, whose devices also carry the
//! repository). The "three-way stale editor END TO END" case, which needs
//! the repository's base-hash reconcile (`savePracticeWithBase`), is
//! `vocara_services/tests/sync_round_trip_three_way.rs`, on two
//! `Persistence` stacks as in Dart.

use vocara_store::sync::sync_transport::FakeCloud;

use crate::device::{Aggregate, Device};
use crate::engine::T0;
use crate::support::*;

struct Pair {
    a: Device,
    b: Device,
}

fn pair() -> Pair {
    let cloud = FakeCloud::new();
    Pair {
        a: Device::new(&cloud, "DEVA", T0),
        b: Device::new(&cloud, "DEVB", T0),
    }
}

impl Pair {
    /// Full sync rounds until quiescent: pushes then pulls on both devices,
    /// repeated so post-pull integrity repairs propagate too; then asserts
    /// both oplogs fully drained (nothing left unsynced).
    fn settle(&self) {
        for _ in 0..4 {
            assert!(!self.a.engine.pump_push().failed());
            assert!(!self.b.engine.pump_push().failed());
            assert!(!self.a.engine.pump_pull().failed());
            assert!(!self.b.engine.pump_pull().failed());
        }
        assert_eq!(self.a.oplog_count(), 0);
        assert_eq!(self.b.oplog_count(), 0);
    }

    fn expect_converged(&self) {
        assert_eq!(self.b.snapshot(), self.a.snapshot());
    }
}

fn title(dev: &Device, practice: &str) -> String {
    col(&dev.db, "practices", practice, "title").unwrap()
}

fn note_content(dev: &Device, note: &str) -> String {
    col(&dev.db, "practice_notes", note, "content").unwrap()
}

fn every_row(agg: &Aggregate) -> [(&'static str, &str); 6] {
    [
        ("practices", &agg.practice),
        ("interpretations", &agg.interp),
        ("assets", &agg.asset),
        ("tracks", &agg.track),
        ("clips", &agg.clip),
        ("practice_notes", &agg.note),
    ]
}

#[test]
fn a_creates_a_practice_with_a_take_and_notes_and_b_pulls_an_identical_aggregate() {
    let p = pair();
    let saved = p.a.seed_practice("Duet");
    assert!(!p.a.engine.pump_push().failed());
    let pull = p.b.engine.pump_pull();
    assert!(!pull.failed());
    assert!(pull.applied > 0);

    // The wire projection of every synced row of the aggregate (practice,
    // interpretation, asset, track, clip, note) is byte-identical on both
    // devices: every row is content-identical.
    for (table, row_id) in every_row(&saved) {
        let on_a = p.a.wire(table, row_id);
        assert!(on_a.is_some(), "{table}");
        assert_eq!(p.b.wire(table, row_id), on_a, "{table}");
    }
    assert_eq!(title(&p.b, &saved.practice), "Duet");
    assert_eq!(
        col::<String>(&p.b.db, "tracks", &saved.track, "display_name").unwrap(),
        "Take 1"
    );
    assert_eq!(
        count(
            &p.b.db,
            "SELECT COUNT(*) FROM clips WHERE track_id = ?",
            &[&saved.track]
        ),
        1
    );
    assert_eq!(note_content(&p.b, &saved.note), "original note");
    // Applying remote changes journaled NOTHING on B (no echo).
    assert_eq!(p.b.oplog_count(), 0);
    p.expect_converged();
}

#[test]
fn concurrent_edits_to_different_rows_of_one_practice_converge_to_the_union() {
    let p = pair();
    let seeded = p.a.seed_practice("Song");
    p.settle();

    // A edits the note; B renames the practice — different rows, no
    // conflict.
    p.a.tx(|| {
        p.a.edit(
            "practice_notes",
            &seeded.note,
            &[("content", &"note edited on A")],
        )
    });
    p.b.tx(|| {
        p.b.edit("practices", &seeded.practice, &[("title", &"renamed on B")])
    });

    p.settle();

    for dev in [&p.a, &p.b] {
        assert_eq!(title(dev, &seeded.practice), "renamed on B");
        assert_eq!(note_content(dev, &seeded.note), "note edited on A");
    }
    p.expect_converged();
}

#[test]
fn concurrent_edits_to_the_same_row_resolve_by_lww_to_the_later_writer() {
    let p = pair();
    let seeded = p.a.seed_practice("Song");
    p.settle();

    p.a.tx(|| {
        p.a.edit("practices", &seeded.practice, &[("title", &"from A")])
    });
    // B's HLC is now strictly greater: B deterministically wins.
    p.b.now.set(p.a.now.get() + 5);
    p.b.tx(|| {
        p.b.edit("practices", &seeded.practice, &[("title", &"from B")])
    });

    p.settle();

    assert_eq!(title(&p.a, &seeded.practice), "from B");
    assert_eq!(title(&p.b, &seeded.practice), "from B");
    p.expect_converged();
}

#[test]
fn tombstone_propagation_leaves_no_zombie_live_children_after_a_stale_child_edit() {
    // A deletes; B pushes a STALE child edit after the delete; no zombie live
    // children survive anywhere.
    let p = pair();
    let seeded = p.a.seed_practice("Song");
    p.settle();

    // A deletes the whole practice (cascade tombstones) and pushes.
    p.a.delete_practice(&seeded.practice);
    assert!(!p.a.engine.pump_push().failed());

    // B, unaware, edits a child track of the now-deleted practice — with a
    // strictly newer HLC, so plain LWW alone would keep the track alive.
    p.b.now.set(p.a.now.get() + 5);
    p.b.tx(|| {
        p.b.edit(
            "tracks",
            &seeded.track,
            &[("display_name", &"zombie rename")],
        )
    });

    // Full exchange, including the extra rounds that propagate each side's
    // integrity repairs (the engine runs them after every pull run).
    p.settle();

    for dev in [&p.a, &p.b] {
        let pid: &str = &seeded.practice;
        // The practice is tombstoned...
        assert_eq!(
            count(
                &dev.db,
                "SELECT COUNT(*) FROM practices WHERE id = ? AND deleted_at IS NULL",
                &[pid]
            ),
            0
        );
        // ...and NOTHING under it is left alive (the applier + the post-pull
        // IntegrityChecker keep the tombstoned-parent invariant).
        for (what, sql) in [
            (
                "tracks",
                "SELECT COUNT(*) FROM tracks WHERE practice_id = ? AND deleted_at IS NULL",
            ),
            (
                "interpretations",
                "SELECT COUNT(*) FROM interpretations WHERE practice_id = ? AND deleted_at IS NULL",
            ),
            (
                "clips",
                "SELECT COUNT(*) FROM clips c JOIN tracks t ON t.id = c.track_id \
                 WHERE t.practice_id = ? AND c.deleted_at IS NULL",
            ),
            (
                "notes",
                "SELECT COUNT(*) FROM practice_notes n JOIN tracks t ON t.id = n.track_id \
                 WHERE t.practice_id = ? AND n.deleted_at IS NULL",
            ),
        ] {
            assert_eq!(count(&dev.db, sql, &[pid]), 0, "no zombie live {what}");
        }
    }
    p.expect_converged();
}

#[test]
fn an_offline_device_catches_up_in_pages_and_converges() {
    let p = pair();
    let seeded = p.a.seed_practice("Song");
    p.settle();

    p.b.transport.set_partitioned(true);

    // Three sequential edits on A, each pushed separately (three log
    // entries).
    p.a.tx(|| {
        p.a.edit("practices", &seeded.practice, &[("title", &"title v2")])
    });
    assert!(!p.a.engine.pump_push().failed());
    p.a.tx(|| {
        p.a.edit("practice_notes", &seeded.note, &[("content", &"note v2")])
    });
    assert!(!p.a.engine.pump_push().failed());
    let second = p.a.add_note(&seeded.track, 900, "second note");
    assert!(!p.a.engine.pump_push().failed());

    // Partitioned: the pull fails and moves nothing.
    let offline = p.b.engine.pump_pull();
    assert!(offline.failed());
    assert_eq!(offline.pages, 0);

    // Reconnect; catch up in pages of 2 (3 changed rows -> 2 pages).
    p.b.transport.set_partitioned(false);
    p.b.transport.set_page_size(2);
    let catch_up = p.b.engine.pump_pull();
    assert!(!catch_up.failed());
    assert_eq!(catch_up.pages, 2, "3 rows at page size 2");
    assert_eq!(catch_up.applied, 3);

    assert_eq!(title(&p.b, &seeded.practice), "title v2");
    let mut notes: Vec<String> =
        p.b.db
            .query(
                "SELECT content FROM practice_notes WHERE track_id = ? ORDER BY content",
                [&seeded.track],
                |r| r.get(0),
            )
            .unwrap();
    notes.sort();
    assert_eq!(notes, ["note v2", "second note"]);
    assert_eq!(note_content(&p.b, &second), "second note");
    p.expect_converged();
}

#[test]
fn the_shared_cloud_holds_each_row_once_per_push_and_the_ack_drains_the_ledger() {
    // Sanity for the harness itself: a seeded aggregate is six journaled
    // rows, pushed parent-first in one batch.
    let p = pair();
    let saved = p.a.seed_practice("Song");
    assert_eq!(p.a.oplog_count(), 6);
    let run = p.a.engine.pump_push();
    assert!(!run.failed());
    assert_eq!(run.pushed, 6);
    let batches = p.a.transport.pushed_batches();
    assert_eq!(batches.len(), 1);
    let tables: Vec<&str> = batches[0].iter().map(|r| r.table()).collect();
    let pos = |t: &str| tables.iter().position(|x| *x == t).unwrap();
    assert!(pos("assets") < pos("clips"));
    assert!(pos("practices") < pos("interpretations"));
    assert!(pos("interpretations") < pos("tracks"));
    assert!(pos("tracks") < pos("clips"));
    assert!(pos("tracks") < pos("practice_notes"));
    assert_eq!(p.a.oplog_count(), 0);
    assert!(p.a.wire("practices", &saved.practice).is_some());
}
