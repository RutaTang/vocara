//! The "three-way stale editor END TO END" case of
//! `client/test/sync_round_trip_test.dart`, which the store's row-level
//! round-trip suite (`vocara_store/tests/sync/round_trip.rs`) leaves out: it
//! needs the repository's base-hash reconcile (`save_practice_with_base`),
//! so it runs here, as Dart's did, on two full `Persistence` stacks
//! (device A and device B) sharing ONE fake cloud.

use std::collections::BTreeMap;
use std::sync::Arc;

use vocara_model::models::audio_track::{AudioTrack, AudioTrackKind};
use vocara_model::models::practice_note::PracticeNote;
use vocara_model::models::song_practice::SongPractice;
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::id::new_id;
use vocara_services::persistence::{AttachSyncOptions, Persistence};
use vocara_store::repositories::SongPracticeRepository;
use vocara_store::sync::sync_engine::SyncEngine;
use vocara_store::sync::sync_hashes::Loaded;
use vocara_store::sync::sync_row::synced_table_registry;
use vocara_store::sync::sync_transport::{FakeCloud, FakeSyncTransport};

struct Device {
    persistence: Persistence,
    engine: Arc<SyncEngine>,
}

impl Device {
    fn new(cloud: &Arc<FakeCloud>, name: &str) -> Device {
        let persistence = Persistence::memory().unwrap();
        let transport = Arc::new(FakeSyncTransport::new(Arc::clone(cloud), name));
        let engine = persistence
            .attach_sync(transport, AttachSyncOptions::default())
            .unwrap();
        Device {
            persistence,
            engine,
        }
    }

    fn repo(&self) -> &dyn SongPracticeRepository {
        self.persistence.song_repo().as_ref()
    }

    fn load(&self, id: &str) -> Loaded<SongPractice> {
        self.repo().load_practice(id).unwrap()
    }

    fn oplog_count(&self) -> i64 {
        self.persistence
            .db()
            .query_row("SELECT COUNT(*) FROM oplog", [], |r| r.get(0))
            .unwrap()
    }

    /// Every synced row: "table/id" -> "hlcPacked|deletedAt". Two converged
    /// devices produce IDENTICAL snapshots (entity-level LWW means equal
    /// winning HLC implies equal row content).
    fn snapshot(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for table in synced_table_registry().keys() {
            let rows = self
                .persistence
                .db()
                .query(
                    &format!("SELECT id, hlc_packed, deleted_at FROM {table}"),
                    [],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, Option<i64>>(2)?,
                        ))
                    },
                )
                .unwrap();
            for (id, packed, deleted) in rows {
                out.insert(
                    format!("{table}/{id}"),
                    format!(
                        "{packed}|{}",
                        deleted.map_or("null".into(), |d| d.to_string())
                    ),
                );
            }
        }
        out
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        self.engine.dispose();
        let _ = std::fs::remove_dir_all(self.persistence.assets().root());
    }
}

struct Pair {
    a: Device,
    b: Device,
    tmp: tempfile::TempDir,
}

impl Pair {
    fn new() -> Pair {
        let cloud = FakeCloud::new();
        Pair {
            a: Device::new(&cloud, "A"),
            b: Device::new(&cloud, "B"),
            tmp: tempfile::tempdir().unwrap(),
        }
    }

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

    /// Seeds a practice on [dev]: a title, one vocal take with a real (tiny)
    /// audio clip, and one note. Returns the saved aggregate.
    fn seed_practice(&self, dev: &Device) -> SongPractice {
        let created = dev.repo().create_practice("Song").unwrap();
        let file = self.tmp.path().join(format!("take_{}.wav", new_id()));
        std::fs::write(&file, [1u8, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        let now = DateTime::now();
        let track = AudioTrack {
            version_id: created.active_version_id.clone(),
            notes: vec![PracticeNote {
                id: new_id(),
                position_ms: 100,
                end_ms: None,
                content: "original note".into(),
                created_at: now,
                updated_at: now,
            }],
            ..AudioTrack::new(new_id(), AudioTrackKind::Vocal, "Take 1", now).with_audio(
                file.to_string_lossy(),
                0,
                1200,
                vec![],
            )
        };
        // Creation flow: the legacy save is the documented path for a fresh
        // aggregate (base := current); editors below always save with base.
        dev.repo()
            .save_practice(&SongPractice {
                vocal_tracks: vec![Arc::new(track)],
                ..created
            })
            .unwrap()
    }
}

/// [pr] with every note's text set to [content].
fn with_note_content(pr: &SongPractice, content: &str) -> SongPractice {
    SongPractice {
        vocal_tracks: pr
            .vocal_tracks
            .iter()
            .map(|t| {
                Arc::new(AudioTrack {
                    notes: t
                        .notes
                        .iter()
                        .map(|n| PracticeNote {
                            content: content.into(),
                            updated_at: DateTime::now(),
                            ..n.clone()
                        })
                        .collect(),
                    ..(**t).clone()
                })
            })
            .collect(),
        ..pr.clone()
    }
}

/// Spins until the wall clock enters the NEXT millisecond, making the HLC
/// order of two subsequent writes on different devices deterministic.
fn next_ms() {
    let start = DateTime::now().millis_since_epoch();
    while DateTime::now().millis_since_epoch() <= start {
        std::thread::yield_now();
    }
}

#[test]
fn three_way_stale_editor_end_to_end_an_edit_synced_in_underneath_an_open_aggregate_survives_its_later_save_on_both_devices(
) {
    let p = Pair::new();
    let seeded = p.seed_practice(&p.a);
    p.settle();

    // B "opens the editor": holds the aggregate + its base hashes in memory.
    let open = p.b.load(&seeded.id);

    // A polishes the note and syncs.
    let la = p.a.load(&seeded.id);
    p.a.repo()
        .save_practice_with_base(
            &with_note_content(&la.aggregate, "polished by A"),
            &la.base_hashes,
        )
        .unwrap();
    assert!(!p.a.engine.pump_push().failed());

    // A's edit lands in B's DATABASE while B's editor still shows the stale
    // aggregate (this is exactly the window three-way reconcile exists for).
    assert!(!p.b.engine.pump_pull().failed());

    // B now saves its STALE aggregate (note still 'original note') with an
    // unrelated edit. The base hashes prove B never touched the note, so the
    // save must keep A's version instead of reverting it.
    next_ms();
    p.b.repo()
        .save_practice_with_base(
            &SongPractice {
                title: "renamed by B".into(),
                ..open.aggregate.clone()
            },
            &open.base_hashes,
        )
        .unwrap();

    p.settle();

    for (name, dev) in [("A", &p.a), ("B", &p.b)] {
        let loaded = dev.load(&seeded.id);
        assert_eq!(
            loaded.aggregate.vocal_tracks[0].notes[0].content, "polished by A",
            "A's edit must survive B's stale save on {name}"
        );
        assert_eq!(loaded.aggregate.vocal_tracks.len(), 1);
        assert_eq!(loaded.aggregate.vocal_tracks[0].notes.len(), 1);
        assert_eq!(loaded.aggregate.title, "renamed by B");
    }
    p.expect_converged();
}
