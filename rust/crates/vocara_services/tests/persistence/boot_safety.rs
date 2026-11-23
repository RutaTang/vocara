//! Port of `client/test/persistence_boot_safety_test.dart`: boot safety of
//! `Persistence::open` — the destructive-sweep gate, the refusal of
//! libraries written by a newer app, the device node id, oplog compaction,
//! the boot clock seed — and the manual `repair_library` pass against the
//! write gate and a running pull.
//!
//! The gate is a door-closer for unrecoverable data loss: sweep_staging
//! deletes the staging tree outright, and a staged WAV can be the ONLY copy
//! of a take (hard-kill mid-record). If boot recovery cannot run — an
//! unreadable or partially corrupt database — every destructive sweep must
//! be skipped, or the sweep destroys exactly the file recovery existed to
//! save.
//!
//! The two pull-run cases are also ported at the sync layer
//! (`vocara_store/tests/sync/boot_safety.rs`, with the repair simulated);
//! here they run through `Persistence::repair_library` itself. Also here:
//! the Persistence case of `database_durability_test.dart` and the
//! compaction half of `oplog_drain_test.dart`'s query-plan case.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags};
use vocara_model::models::audio_track::{AudioTrack, AudioTrackKind};
use vocara_model::models::song_practice::SongPractice;
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::id::new_id;
use vocara_services::persistence::{
    AttachSyncOptions, OpenError, Persistence, WavAnalyzer, NODE_FILE_NAME,
};
use vocara_services::waveform_service::{WaveformResult, SCRATCH_DIR_NAME};
use vocara_store::db::{AssetStore, CURRENT_SCHEMA_VERSION};
use vocara_store::repositories::SongPracticeRepository;
use vocara_store::sync::hlc::Hlc;
use vocara_store::sync::oplog_drain::OplogDrain;
use vocara_store::sync::sync_row::{FieldValue, Fields, SyncRow};
use vocara_store::sync::sync_state_store::SyncStateStore;
use vocara_store::sync::sync_transport::{
    FakeCloud, FakeSyncTransport, FetchPage, PushAck, SyncTransport, TransportError,
};

use crate::support::*;

fn db_path(root: &Path) -> PathBuf {
    root.join("vocara.db")
}

/// Boots once to create the on-disk store, adds one practice, closes.
fn create_practice_and_close(root: &Path) -> String {
    let p = boot(root);
    let id = create_practice(&p).id;
    p.close().unwrap();
    id
}

/// Writes an orphaned recording exactly where the engine streams takes to.
fn stage_orphan(root: &Path, practice_id: &str, bytes: &[u8]) -> PathBuf {
    let f = root.join("audio").join(practice_id).join("take.wav");
    write(&f, bytes);
    f
}

fn raw(root: &Path) -> Connection {
    Connection::open(db_path(root)).unwrap()
}

fn user_version(root: &Path) -> i64 {
    Connection::open_with_flags(db_path(root), OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap()
}

#[test]
fn a_boot_whose_recovery_cannot_run_skips_every_destructive_sweep_so_the_staged_take_survives() {
    let root = tempfile::tempdir().unwrap();
    let practice_id = create_practice_and_close(root.path());
    let staged = stage_orphan(root.path(), &practice_id, &mono_wav(4800));

    // Partial corruption: recovery's reference derivation (and the sweep
    // gate's read probe) needs the clips table; drop it so recovery fails
    // while the database file itself still opens.
    raw(root.path()).execute_batch("DROP TABLE clips").unwrap();

    // Launch must not be blocked (the app comes up degraded)...
    let p = boot(root.path());
    // ...but no sweep may run: before the gate, sweep_staging deleted the
    // staging tree here, destroying the ONLY copy of the take.
    assert!(staged.exists());
    drop(p);
}

#[test]
fn a_healthy_boot_recovers_the_staged_take_and_then_clears_staging() {
    let root = tempfile::tempdir().unwrap();
    let practice_id = create_practice_and_close(root.path());
    let staged = stage_orphan(root.path(), &practice_id, &mono_wav(4800));

    let p = boot(root.path());
    let practice = reload(&p, &practice_id);
    assert_eq!(practice.vocal_tracks.len(), 1);
    assert_eq!(practice.vocal_tracks[0].display_name, "Take 1 (recovered)");
    // Recovered into the pool, so the staging copy is gone (deleted by the
    // recovery itself or reclaimed by the sweep).
    assert!(!staged.exists());
}

#[test]
fn a_healthy_boot_still_clears_unrecoverable_staging_leftovers() {
    let root = tempfile::tempdir().unwrap();
    let practice_id = create_practice_and_close(root.path());
    // Not a WAV at all: never recovered, reclaimed by the staging sweep.
    let leftover = root
        .path()
        .join("audio")
        .join(&practice_id)
        .join("junk.wav");
    write(&leftover, &(0..64u8).collect::<Vec<_>>());

    let _p = boot(root.path());
    assert!(!leftover.exists());
}

#[test]
fn open_refuses_a_library_written_by_a_newer_vocara_and_leaves_it_untouched() {
    let root = tempfile::tempdir().unwrap();
    create_practice_and_close(root.path());
    let newer = CURRENT_SCHEMA_VERSION + 7;
    raw(root.path())
        .pragma_update(None, "user_version", newer)
        .unwrap();

    match boot_with(root.path(), fake_analyze(), None) {
        Err(OpenError::NewerLibrary {
            library_version,
            app_version,
        }) => {
            assert_eq!(library_version, newer);
            assert_eq!(app_version, CURRENT_SCHEMA_VERSION);
        }
        other => panic!("{other:?}"),
    }
    // The refusal itself must not write: the on-disk version is unchanged (a
    // downgrade attempt would freeze users out of their own data).
    assert_eq!(user_version(root.path()), newer);
}

#[test]
fn repair_library_repairs_cross_row_violations_and_reports_counts() {
    let m = Memory::new();
    let practice = create_practice(&m);
    // Fabricate a violation the DB's own FK checks can't see: tombstone the
    // practice row directly, leaving its interpretation live (the repo's
    // delete_practice would cascade, so bypass it).
    m.db()
        .execute(
            "UPDATE practices SET deleted_at = 1 WHERE id = ?",
            [&practice.id],
        )
        .unwrap();
    // An asset nothing references, for the sweep half of the pass — aged
    // past the sweep's 24h grace window (a young unreferenced row is
    // deliberately treated as in-flight, not garbage).
    let src = m.root.join("src.wav");
    write(&src, &mono_wav(100));
    m.assets()
        .import_file(&text(&src), AssetStore::DEFAULT_KIND)
        .unwrap();
    m.db()
        .execute("UPDATE assets SET created_at = 1, updated_at = 1", [])
        .unwrap();
    // Settle the asset's journal entry as a push ack would (the ack
    // hard-deletes): the sweep holds back any asset whose entry is still
    // pending, since a pushed-but-unacked row may already be server-side.
    m.db()
        .execute("DELETE FROM oplog WHERE entity_table = 'assets'", [])
        .unwrap();

    let report = m.repair_library().unwrap();
    assert!(report.violations_found > 0);
    assert_eq!(report.violations_repaired, report.violations_found);
    assert!(report.assets_swept >= 1);

    // Atomicity floor: every repair journals in the SAME transaction as its
    // row mutation, so each repaired (tombstoned) interpretation has a
    // matching oplog entry.
    let interps: Vec<String> = m
        .db()
        .query(
            "SELECT id FROM interpretations WHERE deleted_at IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!interps.is_empty());
    let ops: Vec<String> = m
        .db()
        .query(
            "SELECT entity_id FROM oplog WHERE entity_table = 'interpretations' \
             AND op = 'tombstone'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    for id in &interps {
        assert!(ops.contains(id), "{id} repaired without a journal entry");
    }

    // A second pass finds a clean library.
    let again = m.repair_library().unwrap();
    assert_eq!(again.violations_found, 0);
    assert_eq!(again.violations_repaired, 0);
}

#[test]
fn boot_clears_waveform_intermediates_an_interrupted_analysis_left_after_recoverys_own() {
    // The waveform service removes its .wave scratch file when its analysis
    // ends, which a quit (the budget cuts a stop's finalize off mid-analysis)
    // or a kill never reaches; nothing else ever looked in waveform_cache/
    // again.
    let root = tempfile::tempdir().unwrap();
    let practice_id = create_practice_and_close(root.path());
    stage_orphan(root.path(), &practice_id, &mono_wav(4800));
    let scratch = root.path().join(SCRATCH_DIR_NAME);
    fs::create_dir_all(&scratch).unwrap();
    let stale = scratch.join("interrupted.wave");
    fs::write(&stale, [1u8; 64]).unwrap();

    // Recovery analyzes through the same scratch folder: its intermediate
    // must be left alone for as long as its analysis runs.
    let saw_own_scratch = Arc::new(AtomicBool::new(false));
    let saw = saw_own_scratch.clone();
    let own_dir = scratch.clone();
    let analyze_with_scratch: WavAnalyzer = Arc::new(move |_| {
        let own = own_dir.join("recovering.wave");
        fs::write(&own, [2u8; 64]).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        saw.store(own.exists(), Ordering::SeqCst);
        fs::remove_file(&own).unwrap();
        Ok(WaveformResult {
            peaks: vec![0.1, 0.2],
            duration_ms: 500,
        })
    });

    let _p = boot_with(root.path(), analyze_with_scratch, None).unwrap();
    assert!(saw_own_scratch.load(Ordering::SeqCst));
    assert!(!stale.exists());
}

#[test]
fn a_systemic_analyzer_failure_exempts_staged_takes_so_the_next_boot_recovers_them() {
    let root = tempfile::tempdir().unwrap();
    let practice_id = create_practice_and_close(root.path());
    let staged = stage_orphan(root.path(), &practice_id, &mono_wav(4800));

    // Every recovery attempt fails (broken decoder / disk): before the
    // exemption, the sweep then trashed ALL orphans in the same boot and
    // recovery never retried (staging empty next boot).
    let sick = boot_with(root.path(), broken_analyze(), None).unwrap();
    sick.close().unwrap();
    assert!(staged.exists()); // spared for a retry

    // Next boot, decoder healthy: the take is recovered normally.
    let healthy = boot(root.path());
    assert_eq!(reload(&healthy, &practice_id).vocal_tracks.len(), 1);
    assert!(!staged.exists());
}

#[test]
fn repair_library_started_during_an_in_flight_save_waits_for_it() {
    // And cannot tombstone the asset the save just imported.
    let m = Memory::new();
    let practice = create_practice(&m);
    let src = m.root.join("take.wav");
    write(&src, &mono_wav(200));
    let asset_live = |id: &str| -> bool {
        m.db()
            .query_row("SELECT deleted_at FROM assets WHERE id = ?", [id], |r| {
                r.get::<_, Option<i64>>(0)
            })
            .unwrap()
            .is_none()
    };

    // The repository save shape under the gate's shared side: the asset
    // import lands first, the rows referencing it commit later — hold the
    // gate open across that window.
    let events: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let (imported_tx, imported) = mpsc::channel::<String>();
    let (release, hold_save) = mpsc::channel::<()>();
    std::thread::scope(|scope| {
        let (m, events, src, practice) = (&m, &events, &src, &practice);
        let save = scope.spawn(move || {
            m.write_gate().shared(|| {
                let id = m
                    .assets()
                    .import_file(&text(src), AssetStore::DEFAULT_KIND)
                    .unwrap();
                // Age the fresh row past the sweep's 24 h grace window.
                // Without this the age guard alone kept it live and the
                // window checks below proved nothing about the gate: now
                // ONLY the gate stands between this unreferenced row and the
                // sweep.
                m.db()
                    .execute("UPDATE assets SET created_at = 1, updated_at = 1", [])
                    .unwrap();
                events.lock().unwrap().push("imported");
                imported_tx.send(id.clone()).unwrap();
                hold_save.recv().unwrap();
                // The rows referencing the import commit last (nested shared
                // section; the gate is reader-preferring, so the queued
                // repair cannot block it).
                let pooled = m.assets().resolve(&id).unwrap().unwrap();
                let saved = m.song_repo().save_practice(&SongPractice {
                    vocal_tracks: vec![Arc::new(AudioTrack {
                        version_id: practice.active_version_id.clone(),
                        ..AudioTrack::new(
                            new_id(),
                            AudioTrackKind::Vocal,
                            "Take 1",
                            DateTime::local_ymd(2026, 1, 1),
                        )
                        .with_audio(pooled, 0, 4, Vec::new())
                    })],
                    ..practice.clone()
                });
                assert!(saved.is_ok(), "{saved:?}");
                events.lock().unwrap().push("save-committed");
            })
        });
        let asset_id = imported.recv().unwrap();

        let repair = scope.spawn(|| {
            let report = m.repair_library();
            events.lock().unwrap().push("repair-done");
            report
        });
        std::thread::sleep(Duration::from_millis(25));
        // The exclusive pass must still be queued behind the in-flight
        // save...
        assert_eq!(*events.lock().unwrap(), vec!["imported"]);
        // ...so the imported, not-yet-referenced (and aged) asset is
        // untouched.
        assert!(asset_live(&asset_id));

        release.send(()).unwrap();
        save.join().unwrap();
        repair.join().unwrap().unwrap();
        assert_eq!(
            *events.lock().unwrap(),
            vec!["imported", "save-committed", "repair-done"]
        );
        // Once the repair does run, the committed clip row references the
        // asset.
        assert!(asset_live(&asset_id));
    });
}

/// Holds a pull run between two pages: the fetch made when the inner
/// transport has served [`PausingTransport::pause_after`] fetches signals
/// `paused` and waits on `resume` before serving its page.
struct PausingTransport {
    inner: Arc<FakeSyncTransport>,
    pause_after: Mutex<Option<usize>>,
    paused: Mutex<Option<mpsc::Sender<()>>>,
    resume: Mutex<Option<mpsc::Receiver<()>>>,
}

impl SyncTransport for PausingTransport {
    fn push_rows(&self, rows: &[SyncRow]) -> Result<PushAck, TransportError> {
        self.inner.push_rows(rows)
    }

    fn fetch_changes(&self, since_token: Option<&str>) -> Result<FetchPage, TransportError> {
        let pause = {
            let mut at = self.pause_after.lock().unwrap();
            if *at == Some(self.inner.fetch_calls()) {
                *at = None;
                true
            } else {
                false
            }
        };
        if pause {
            if let Some(paused) = self.paused.lock().unwrap().take() {
                let _ = paused.send(());
            }
            let resume = self.resume.lock().unwrap().take();
            if let Some(resume) = resume {
                let _ = resume.recv();
            }
        }
        self.inner.fetch_changes(since_token)
    }

    fn push_asset_bytes(&self, hash: &str, path: &Path) -> Result<(), TransportError> {
        self.inner.push_asset_bytes(hash, path)
    }

    fn fetch_asset_bytes(&self, hash: &str, path: &Path) -> Result<(), TransportError> {
        self.inner.fetch_asset_bytes(hash, path)
    }
}

struct Paused {
    inner: Arc<FakeSyncTransport>,
    wire: Arc<PausingTransport>,
    paused: mpsc::Receiver<()>,
    resume: mpsc::Sender<()>,
}

fn pausing_transport() -> Paused {
    let inner = Arc::new(FakeSyncTransport::new(FakeCloud::new(), "device"));
    let (paused_tx, paused) = mpsc::channel();
    let (resume, resume_rx) = mpsc::channel();
    Paused {
        wire: Arc::new(PausingTransport {
            inner: inner.clone(),
            pause_after: Mutex::new(None),
            paused: Mutex::new(Some(paused_tx)),
            resume: Mutex::new(Some(resume_rx)),
        }),
        inner,
        paused,
        resume,
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn fields(pairs: Vec<(&str, FieldValue)>) -> Fields {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

#[test]
fn repair_library_started_between_two_pages_of_a_pull_run_waits_for_the_run() {
    // So it never repairs a half-delivered aggregate.
    let m = Memory::new();
    let Paused {
        inner,
        wire,
        paused,
        resume,
    } = pausing_transport();
    inner.set_page_size(1);
    let engine = m
        .attach_sync(wire.clone(), AttachSyncOptions::default())
        .unwrap();
    let peer = format!("{:0<26}", "PEER");
    let t = now_ms() - 100000;
    let cols = |wall: i64, live: bool| -> Vec<(&str, FieldValue)> {
        vec![
            ("created_at", t.into()),
            ("updated_at", wall.into()),
            ("deleted_at", if live { None } else { Some(wall) }.into()),
            ("hlc_wall", wall.into()),
            ("hlc_ctr", 0i64.into()),
            ("hlc_node", peer.clone().into()),
            (
                "hlc_packed",
                Hlc::new(wall, 0, peer.clone()).packed().into(),
            ),
            ("schema_v", 1i64.into()),
        ]
    };
    let practice_id = new_id();
    let interp_id = new_id();
    let practice = |wall: i64, live: bool| {
        let mut f = vec![
            ("id", practice_id.clone().into()),
            ("title", "song".into()),
            ("timeline_duration_ms", 0i64.into()),
            ("master_volume", 1.0.into()),
            ("active_interpretation_id", FieldValue::Null),
            ("separation_asset_id", FieldValue::Null),
            ("reverb_json", FieldValue::Null),
            ("lyrics_json", FieldValue::Null),
        ];
        f.extend(cols(wall, live));
        SyncRow::new("practices", fields(f))
    };
    let interp = |wall: i64, live: bool| {
        let mut f = vec![
            ("id", interp_id.clone().into()),
            ("practice_id", practice_id.clone().into()),
            ("name", "take".into()),
            ("order_index", 0i64.into()),
        ];
        f.extend(cols(wall, live));
        SyncRow::new("interpretations", fields(f))
    };
    let interp_deleted_at = || -> Option<i64> {
        m.db()
            .query_row(
                "SELECT deleted_at AS d FROM interpretations WHERE id = ?",
                [&interp_id],
                |r| r.get(0),
            )
            .unwrap()
    };

    // Shared history: the practice and its interpretation, then deleted.
    let cloud = inner.cloud().clone();
    cloud.accept(practice(t + 1, true));
    cloud.accept(interp(t + 2, true));
    cloud.accept(practice(t + 3, false));
    cloud.accept(interp(t + 4, false));
    assert!(!engine.pump_pull().failed());

    // A peer revives both. The interpretation's revive lands on one page, the
    // practice's on the next — between them the store holds a live
    // interpretation under a tombstoned practice, which is pending, not
    // broken.
    cloud.accept(interp(t + 5, true));
    cloud.accept(practice(t + 6, true));
    let events: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    *wire.pause_after.lock().unwrap() = Some(inner.fetch_calls() + 1);
    std::thread::scope(|scope| {
        let pull = scope.spawn(|| {
            let run = engine.pump_pull();
            events.lock().unwrap().push("pull-done");
            run
        });
        paused.recv().unwrap();
        assert_eq!(interp_deleted_at(), None, "page 1 landed");

        let repair = scope.spawn(|| {
            let report = m.repair_library();
            events.lock().unwrap().push("repair-done");
            report
        });
        std::thread::sleep(Duration::from_millis(25));
        assert!(
            events.lock().unwrap().is_empty(),
            "the repair must wait for the run"
        );

        resume.send(()).unwrap();
        assert!(!pull.join().unwrap().failed());
        repair.join().unwrap().unwrap();
    });
    assert_eq!(*events.lock().unwrap(), vec!["pull-done", "repair-done"]);
    assert_eq!(
        interp_deleted_at(),
        None,
        "the revived interpretation must not be tombstoned on a verdict about a \
         half-delivered aggregate"
    );
    assert_eq!(count(&m, "SELECT COUNT(*) FROM oplog", &[]), 0);
    engine.dispose();
}

#[test]
fn a_repair_waiting_for_a_pull_run_does_not_hold_up_saves_meanwhile() {
    // The repair took the write gate first and only then queued behind the
    // pull: for the whole run every save, import and take (each a shared
    // holder of the gate) stalled behind it.
    let m = Memory::new();
    let Paused {
        inner,
        wire,
        paused,
        resume,
    } = pausing_transport();
    let engine = m
        .attach_sync(wire.clone(), AttachSyncOptions::default())
        .unwrap();
    *wire.pause_after.lock().unwrap() = Some(inner.fetch_calls());
    std::thread::scope(|scope| {
        let pull = scope.spawn(|| engine.pump_pull());
        paused.recv().unwrap();
        let repair = scope.spawn(|| m.repair_library());
        std::thread::sleep(Duration::from_millis(10));

        let (saved_tx, saved) = mpsc::channel();
        let gate = m.write_gate().clone();
        scope.spawn(move || {
            let _ = saved_tx.send(gate.shared(|| true));
        });
        assert_eq!(
            saved.recv_timeout(Duration::from_secs(1)),
            Ok(true),
            "a save must not wait for the pull run"
        );

        resume.send(()).unwrap();
        assert!(!pull.join().unwrap().failed());
        repair.join().unwrap().unwrap();
    });
    engine.dispose();
}

#[test]
fn a_second_sync_engine_is_refused() {
    let m = Memory::new();
    let wire = Arc::new(FakeSyncTransport::new(FakeCloud::new(), "device"));
    let first = m
        .attach_sync(wire.clone(), AttachSyncOptions::default())
        .unwrap();
    assert!(Arc::ptr_eq(&m.sync().unwrap(), &first));
    assert!(matches!(
        m.attach_sync(wire, AttachSyncOptions::default()),
        Err(vocara_store::Error::State(_))
    ));
}

#[test]
fn the_device_node_id_survives_restarts() {
    let root = tempfile::tempdir().unwrap();
    boot(root.path()).close().unwrap();
    let node_file = root.path().join(NODE_FILE_NAME);
    let first = fs::read_to_string(&node_file).unwrap().trim().to_string();
    assert_eq!(first.len(), 26);

    boot(root.path()).close().unwrap();
    assert_eq!(fs::read_to_string(&node_file).unwrap().trim(), first);
}

#[test]
fn an_empty_or_corrupt_device_node_is_treated_as_absent_and_a_fresh_id_minted() {
    // Instead of silently adopting a truncated identity: a power cut mid-write
    // under the old non-durable write could leave exactly these shapes.
    let root = tempfile::tempdir().unwrap();
    let node_file = root.path().join(NODE_FILE_NAME);
    fs::write(&node_file, "").unwrap();
    boot(root.path()).close().unwrap();
    let minted = fs::read_to_string(&node_file).unwrap().trim().to_string();
    assert_eq!(minted.len(), 26);

    // A truncated id is corrupt, not a shorter identity.
    fs::write(&node_file, "01ABC").unwrap();
    boot(root.path()).close().unwrap();
    let reminted = fs::read_to_string(&node_file).unwrap().trim().to_string();
    assert_eq!(reminted.len(), 26);
    assert_ne!(reminted, "01ABC");

    // Undecodable bytes are absent too.
    fs::write(&node_file, [0xFFu8, 0xFE, 0x41]).unwrap();
    boot(root.path()).close().unwrap();
    let decoded = fs::read_to_string(&node_file).unwrap().trim().to_string();
    assert_eq!(decoded.len(), 26);

    // A valid id keeps being preserved byte-for-byte.
    boot(root.path()).close().unwrap();
    assert_eq!(fs::read_to_string(&node_file).unwrap().trim(), decoded);
    assert!(!root.path().join("device_node.tmp").exists());
}

// --- Oplog compaction ---------------------------------------------------------

/// Journals one entry through the production chokepoint (no row mutation
/// needed — compaction is about the ledger alone).
fn journal(p: &Persistence, table: &str, id: &str, op: &str, depends_on: Option<&str>) -> Hlc {
    let s = p.writer().stamp();
    p.writer()
        .journal(
            table,
            id,
            op,
            &s.hlc,
            if op == "tombstone" { "" } else { "h" },
            depends_on,
        )
        .unwrap();
    s.hlc
}

fn oplog_count(p: &Persistence) -> i64 {
    count(p, "SELECT COUNT(*) FROM oplog", &[])
}

fn pad(s: &str) -> String {
    format!("{s:0<26}")
}

#[test]
fn compaction_keeps_only_the_latest_op_per_entity_and_the_drain_ships_what_it_would_have() {
    let m = Memory::new();
    let (id1, id2) = (pad("P1"), pad("P2"));
    journal(&m, "practices", &id1, "insert", None);
    journal(&m, "practices", &id1, "update", None);
    journal(&m, "practices", &id2, "insert", None);
    let tomb = journal(&m, "practices", &id1, "tombstone", None);
    assert_eq!(oplog_count(&m), 4);

    assert_eq!(m.compact_oplog().unwrap(), 2);
    assert_eq!(oplog_count(&m), 2);

    // The drain result is IDENTICAL to an uncompacted drain: latest op per
    // entity, trailing tombstone preserved (its collapse rule is exactly what
    // compaction enforced in place).
    let batch = OplogDrain::new(m.db().clone())
        .read_pending(OplogDrain::DEFAULT_LIMIT)
        .unwrap();
    assert_eq!(batch.entries.len(), 2);
    assert_eq!(batch.entries[0].id, id2);
    assert_eq!(batch.entries[0].op, "insert");
    assert_eq!(batch.entries[1].id, id1);
    assert_eq!(batch.entries[1].op, "tombstone");
    assert_eq!(batch.entries[1].hlc_packed, tomb.packed());
    assert_eq!(batch.max_seq, 4, "surviving seqs keep their values");
}

#[test]
fn compaction_carries_the_latest_dependency_onto_the_surviving_entry() {
    // A content-only update must not shed the asset gate.
    let m = Memory::new();
    let id = pad("C1");
    let asset = pad("AX");
    journal(&m, "clips", &id, "insert", Some(&asset));
    journal(&m, "clips", &id, "update", None); // no dependency of its own

    assert_eq!(m.compact_oplog().unwrap(), 1);
    let batch = OplogDrain::new(m.db().clone())
        .read_pending(OplogDrain::DEFAULT_LIMIT)
        .unwrap();
    assert_eq!(batch.entries.len(), 1);
    assert_eq!(batch.entries[0].op, "update");
    assert_eq!(
        batch.entries[0].depends_on_asset_id.as_deref(),
        Some(asset.as_str()),
        "the gate must survive compaction exactly as it survives the drain collapse"
    );
}

#[test]
fn with_no_dependency_anywhere_compaction_deletes_history_and_rewrites_no_survivor() {
    // The carry has nothing to carry when no entry names an asset — yet it
    // used to rewrite every surviving entry, on every boot.
    let m = Memory::new();
    let total_changes = || -> i64 {
        m.db()
            .query_row("SELECT total_changes() AS n", [], |r| r.get(0))
            .unwrap()
    };
    let (id1, id2) = (pad("P1"), pad("P2"));
    journal(&m, "practices", &id1, "insert", None);
    journal(&m, "practices", &id1, "update", None);
    journal(&m, "practices", &id2, "insert", None);

    let before = total_changes();
    assert_eq!(m.compact_oplog().unwrap(), 1);
    assert_eq!(total_changes() - before, 1, "the one deletion");
    let batch = OplogDrain::new(m.db().clone())
        .read_pending(OplogDrain::DEFAULT_LIMIT)
        .unwrap();
    let pairs: Vec<(String, String)> = batch
        .entries
        .iter()
        .map(|e| (e.id.clone(), e.op.clone()))
        .collect();
    assert_eq!(
        pairs,
        vec![(id1, "update".to_string()), (id2, "insert".to_string())]
    );
}

#[test]
fn compaction_is_idempotent_and_a_no_op_on_an_already_compact_ledger() {
    let m = Memory::new();
    journal(&m, "practices", &pad("P1"), "insert", None);
    assert_eq!(m.compact_oplog().unwrap(), 0);
    assert_eq!(m.compact_oplog().unwrap(), 0);
    assert_eq!(oplog_count(&m), 1);
}

#[test]
fn the_boot_maintenance_pass_compacts_superseded_history() {
    // Boot #1: a real practice (several entities, one entry each) plus a
    // superseded extra op for the practice row itself.
    let root = tempfile::tempdir().unwrap();
    let first = boot(root.path());
    let id = create_practice(&first).id;
    let s = first.writer().stamp();
    first
        .writer()
        .journal("practices", &id, "update", &s.hlc, "h2", None)
        .unwrap();
    let journaled = |p: &Persistence| {
        count(
            p,
            "SELECT COUNT(*) FROM oplog WHERE entity_table = 'practices' AND entity_id = ?",
            &[&id],
        )
    };
    assert_eq!(journaled(&first), 2);
    first.close().unwrap();

    // Boot #2 runs the gated maintenance pass, which compacts.
    let second = boot(root.path());
    assert_eq!(journaled(&second), 1);
    let op: String = second
        .db()
        .query_row(
            "SELECT MAX(op) FROM oplog WHERE entity_table = 'practices' AND entity_id = ?",
            [&id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(op, "update", "the LATEST op won");
}

#[test]
fn the_compaction_carry_forward_probes_each_entity_through_ix_oplog_entity() {
    // Instead of scanning the whole ledger per entry (oplog_drain_test's
    // plan case; the ack's half is the drain's own test). Unindexed, the
    // correlated subquery reads `SCAN o`: one full ledger pass per surviving
    // entry, quadratic in a ledger nothing acks today.
    let m = Memory::new();
    for i in 0..50 {
        let id = pad(&format!("C{i}"));
        journal(&m, "clips", &id, "insert", None);
        journal(&m, "clips", &id, "update", None);
    }
    let steps: Vec<String> = m
        .db()
        .query(
            &format!("EXPLAIN QUERY PLAN {}", Persistence::COMPACT_CARRY_SQL),
            [],
            |r| r.get::<_, String>("detail"),
        )
        .unwrap();
    assert!(
        steps
            .iter()
            .any(|s| s.starts_with("SEARCH o USING INDEX ix_oplog_entity")),
        "{}",
        steps.join("\n")
    );
    assert!(!steps.iter().any(|s| s == "SCAN o"), "{}", steps.join("\n"));
}

// --- Durability ----------------------------------------------------------------

#[test]
fn the_live_store_flushes_every_commit_and_checkpoint_through_to_the_drive() {
    // fullfsync on. On macOS SQLite's default sync is a plain fsync, which
    // leaves the bytes in the drive's write cache: a power cut could lose
    // saves that already succeeded, or reorder a checkpoint's writes and
    // corrupt the library.
    let root = tempfile::tempdir().unwrap();
    let analyze: WavAnalyzer = Arc::new(|_| {
        Ok(WaveformResult {
            peaks: Vec::new(),
            duration_ms: 0,
        })
    });
    let p = boot_with(root.path(), analyze, None).unwrap();
    let pragma = |p: &Persistence| -> i64 {
        p.db()
            .query_row("PRAGMA fullfsync", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(pragma(&p), 1);
    // The flag is per connection: the in-memory store wires the same open.
    let memory = Memory::new();
    assert_eq!(pragma(&memory), 1);
}

// --- Boot clock seed -------------------------------------------------------------

/// A stamp from a peer, ahead of this device's wall clock but well inside the
/// plausibility window the applier's drift gate allows.
fn peer_stamp(ahead_ms: i64) -> Hlc {
    Hlc::new(now_ms() + ahead_ms, 0, pad("REMOTE"))
}

#[test]
fn a_verified_remote_stamp_whose_record_was_parked_still_seeds_the_clock_after_a_restart() {
    // The applier merges the stamps of records the FK sweep parks — HLC
    // semantics advance on any received message — but a parked record leaves
    // no row behind, and sync_quarantine's stamp column is deliberately not
    // named hlc_packed (it also holds suspect far-future stamps, which must
    // never seed a clock). So that legitimate high-water mark used to live
    // only in memory: after a restart a local edit could take a LOWER stamp,
    // and the quarantine replay would then win LWW and silently overwrite
    // the user's newer edit.
    let root = tempfile::tempdir().unwrap();
    let first = boot(root.path());
    let parked = peer_stamp(120000);
    SyncStateStore::new(first.db().clone())
        .raise_hlc_high_water(&parked.packed())
        .unwrap();
    // Nothing in the tables carries that stamp — the seed's other source.
    first.close().unwrap();

    let second = boot(root.path());
    let local = second.writer().stamp().hlc;
    assert!(
        local > parked,
        "a local edit after the restart must out-stamp what this device already \
         accepted: got {} vs {}",
        local.packed(),
        parked.packed()
    );
}

#[test]
fn the_clock_seeds_from_the_highest_stamp_in_the_tables_and_skips_a_malformed_one() {
    // Every synced row and the oplog carry hlc_packed: the global MAX is the
    // greatest stamp this install ever produced or adopted. One malformed
    // value only skips its own table.
    let root = tempfile::tempdir().unwrap();
    let first = boot(root.path());
    let ahead = peer_stamp(90000);
    let practice = create_practice(&first);
    first
        .db()
        .execute(
            "UPDATE practices SET hlc_packed = ? WHERE id = ?",
            [ahead.packed(), practice.id.clone()],
        )
        .unwrap();
    first
        .db()
        .execute("UPDATE interpretations SET hlc_packed = 'not a stamp'", [])
        .unwrap();
    first.close().unwrap();

    let second = boot(root.path());
    let local = second.writer().stamp().hlc;
    assert!(local > ahead, "{} vs {}", local.packed(), ahead.packed());
}

#[test]
fn the_durable_mark_only_ever_moves_up() {
    let root = tempfile::tempdir().unwrap();
    let p = boot(root.path());
    let state = SyncStateStore::new(p.db().clone());
    let high = peer_stamp(120000);
    let low = peer_stamp(1000);

    state.raise_hlc_high_water(&high.packed()).unwrap();
    state.raise_hlc_high_water(&low.packed()).unwrap();
    assert_eq!(state.hlc_high_water().unwrap(), Some(high.packed()));
}
