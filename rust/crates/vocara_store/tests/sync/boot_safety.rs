//! The sync parts of `client/test/persistence_boot_safety_test.dart` that
//! need only the sync layer: a whole-store repair waits for a pull run
//! instead of judging its half-delivered aggregate, and a repair queued
//! behind a pull run does not hold up local saves.
//!
//! `Persistence::repair_library` lives above the store (in
//! `vocara_services::persistence`, whose tests run these two cases through
//! it); what it does with the engine is
//! `engine.run_exclusive(|| db.transaction(|| checker.repair()))` (its
//! write-gate and asset sweeps are Persistence's), which is what these tests
//! run. Not ported here: the boot clock seed tests (Persistence's boot path;
//! the durable mark itself is covered by `sync_state_store`), and everything
//! about recovery, migration and the single-instance lock — all in
//! `vocara_services/tests/persistence`.

use std::path::Path;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use vocara_model::util::id::new_id;
use vocara_store::sync::hlc::Hlc;
use vocara_store::sync::sync_row::SyncRow;
use vocara_store::sync::sync_transport::{
    FakeCloud, FakeSyncTransport, FetchPage, PushAck, SyncTransport, TransportError,
};

use crate::device::Device;
use crate::engine::T0;
use crate::support::*;

/// Holds a pull run between two pages: the fetch made when the inner
/// transport has served [pause_after] fetches signals [paused] and waits on
/// [resume] before serving its page.
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
            let mut at = self.pause_after.lock();
            if *at == Some(self.inner.fetch_calls()) {
                *at = None;
                true
            } else {
                false
            }
        };
        if pause {
            if let Some(paused) = self.paused.lock().take() {
                let _ = paused.send(());
            }
            let resume = self.resume.lock().take();
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
    device: Device,
    wire: Arc<PausingTransport>,
    paused: mpsc::Receiver<()>,
    resume: mpsc::Sender<()>,
}

fn pausing_device() -> Paused {
    let inner = Arc::new(FakeSyncTransport::new(FakeCloud::new(), "device"));
    let (paused_tx, paused) = mpsc::channel();
    let (resume, resume_rx) = mpsc::channel();
    let wire = Arc::new(PausingTransport {
        inner: inner.clone(),
        pause_after: Mutex::new(None),
        paused: Mutex::new(Some(paused_tx)),
        resume: Mutex::new(Some(resume_rx)),
    });
    Paused {
        device: Device::with_transport(inner, wire.clone(), "DEVICE", T0),
        wire,
        paused,
        resume,
    }
}

/// Persistence.repairLibrary's integrity pass, as it runs against the
/// engine.
fn repair_library(device: &Device) {
    let checker = device.checker();
    let db = device.db.clone();
    device
        .engine
        .run_exclusive(|| db.transaction(|| checker.repair()))
        .unwrap();
}

#[test]
fn a_repair_started_between_two_pages_of_a_pull_run_waits_for_the_run() {
    // So it never repairs a half-delivered aggregate.
    let Paused {
        device,
        wire,
        paused,
        resume,
    } = pausing_device();
    device.transport.set_page_size(1);
    let peer = id("PEER");
    let t = T0 - 100000;
    let cols = |wall: i64, live: bool| {
        let h = Hlc::new(wall, 0, peer.clone());
        fields! {
            "created_at" => t,
            "updated_at" => wall,
            "deleted_at" => if live { None } else { Some(wall) },
            "hlc_wall" => wall,
            "hlc_ctr" => 0i64,
            "hlc_node" => peer.clone(),
            "hlc_packed" => h.packed(),
            "schema_v" => 1i64,
        }
    };
    let practice_id = new_id();
    let interp_id = new_id();
    let practice = |wall: i64, live: bool| {
        SyncRow::new(
            "practices",
            merged(
                fields! {
                    "id" => practice_id.clone(),
                    "title" => "song",
                    "timeline_duration_ms" => 0i64,
                    "master_volume" => 1.0,
                    "active_interpretation_id" => NULL,
                    "separation_asset_id" => NULL,
                    "reverb_json" => NULL,
                    "lyrics_json" => NULL,
                },
                cols(wall, live),
            ),
        )
    };
    let interp = |wall: i64, live: bool| {
        SyncRow::new(
            "interpretations",
            merged(
                fields! {
                    "id" => interp_id.clone(),
                    "practice_id" => practice_id.clone(),
                    "name" => "take",
                    "order_index" => 0i64,
                },
                cols(wall, live),
            ),
        )
    };
    let db = device.db.clone();
    let interp_deleted_at =
        || col::<Option<i64>>(&db, "interpretations", &interp_id, "deleted_at").unwrap();

    // Shared history: the practice and its interpretation, then deleted.
    let cloud = device.transport.cloud().clone();
    cloud.accept(practice(t + 1, true));
    cloud.accept(interp(t + 2, true));
    cloud.accept(practice(t + 3, false));
    cloud.accept(interp(t + 4, false));
    assert!(!device.engine.pump_pull().failed());

    // A peer revives both. The interpretation's revive lands on one page, the
    // practice's on the next — between them the store holds a live
    // interpretation under a tombstoned practice, which is pending, not
    // broken.
    cloud.accept(interp(t + 5, true));
    cloud.accept(practice(t + 6, true));
    let events: Arc<Mutex<Vec<&'static str>>> = Arc::default();
    *wire.pause_after.lock() = Some(device.transport.fetch_calls() + 1);
    let device = Arc::new(device);
    let pull = {
        let (device, events) = (device.clone(), events.clone());
        std::thread::spawn(move || {
            let run = device.engine.pump_pull();
            events.lock().push("pull-done");
            run
        })
    };
    paused.recv().unwrap();
    assert_eq!(interp_deleted_at(), None, "page 1 landed");

    let repair = {
        let (device, events) = (device.clone(), events.clone());
        std::thread::spawn(move || {
            repair_library(&device);
            events.lock().push("repair-done");
        })
    };
    std::thread::sleep(Duration::from_millis(25));
    assert!(events.lock().is_empty(), "the repair must wait for the run");

    resume.send(()).unwrap();
    assert!(!pull.join().unwrap().failed());
    repair.join().unwrap();
    assert_eq!(*events.lock(), ["pull-done", "repair-done"]);
    assert_eq!(
        interp_deleted_at(),
        None,
        "the revived interpretation must not be tombstoned on a verdict about a half-delivered \
         aggregate"
    );
    assert_eq!(device.oplog_count(), 0);
}

#[test]
fn a_repair_waiting_for_a_pull_run_does_not_hold_up_saves_meanwhile() {
    // In Dart the repair took the write gate first and only then queued
    // behind the pull: for the whole run every save stalled behind it. Here
    // the store side of the same property: neither a paused pull run nor a
    // repair queued behind it holds the database.
    let Paused {
        device,
        wire,
        paused,
        resume,
    } = pausing_device();
    *wire.pause_after.lock() = Some(device.transport.fetch_calls());
    let device = Arc::new(device);
    let pull = {
        let device = device.clone();
        std::thread::spawn(move || device.engine.pump_pull())
    };
    paused.recv().unwrap();
    let repair = {
        let device = device.clone();
        std::thread::spawn(move || repair_library(&device))
    };
    std::thread::sleep(Duration::from_millis(10));

    let (saved_tx, saved) = mpsc::channel();
    {
        let device = device.clone();
        std::thread::spawn(move || {
            device.create_practice("saved meanwhile");
            let _ = saved_tx.send(());
        });
    }
    assert!(
        saved.recv_timeout(Duration::from_secs(1)).is_ok(),
        "a save must not wait for the pull run"
    );

    resume.send(()).unwrap();
    assert!(!pull.join().unwrap().failed());
    repair.join().unwrap();
}
