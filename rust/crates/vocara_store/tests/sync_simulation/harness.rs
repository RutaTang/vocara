//! Seeded randomized convergence simulation for the P0 sync stack (port of
//! `client/test/support/sync_sim_harness.dart`) — the Automerge/Ditto-style
//! "fuzz the whole engine, assert convergence at quiescence" suite.
//!
//! One episode = N devices (2..4), each a REAL persistence stack (SQLite
//! library + repositories + SyncWriter + SyncEngine) over one shared
//! [`FakeCloud`]. A seeded op mix drives the REPOSITORY layer with realistic
//! editor behavior (held aggregates + base hashes across other devices'
//! syncs — the exact window three-way reconcile exists for), interleaved
//! with seeded transport faults (partitions, paging, held pushes, injected
//! push conflicts, verbatim page re-delivery, killed page commits).
//!
//! The stack is the one `Persistence` wires (Dart built `Persistence` from
//! these parts; the Rust `Persistence` has no constructor that takes a
//! clock, so the parts are wired here exactly as `Persistence::wire` and
//! `Persistence::attach_sync` wire them: one `SyncRowBuilder` shared by the
//! engine and an applier with a persistent quarantine, the oplog drain, the
//! sync state, the integrity checker, and the test-only page-commit hook).
//!
//! DETERMINISM: every decision comes from one `Random(seed)` — Dart's VM
//! algorithm, reimplemented bit for bit ([`DartRandom`]), so each seed draws
//! the same decision sequence as its Dart episode; every HLC stamp comes
//! from per-device logical clocks (`episode logical time + a per-device
//! skew`), and every row id is minted by the harness. The one deliberate
//! exception: repositories internally stamp `created_at`/`updated_at`/
//! `deleted_at` VALUES from the wall clock and asset ids from `new_id()` —
//! neither feeds back into any control flow the harness exercises (target
//! selection is registry-index based, LWW is HLC-only), so the op sequence,
//! the merge decisions and the final convergence outcome are reproducible
//! from the seed alone. Differing skews make HLC order genuinely diverge from
//! episode order, so "later real-time edit loses LWW" cases are exercised
//! for real. (The episodes are the Dart ones as long as the stack decides
//! as the Dart stack did; a replayed Dart op log is the reference when they
//! part.)
//!
//! Held pushes: a Dart `pumpPush()` parked behind the transport's hold was a
//! pending future; here it is a pump running on its own thread, parked
//! inside `push_rows` until the harness releases it. The harness waits until
//! the pump has parked (or finished without reaching the wire: nothing to
//! push, or partitioned) before the next op, so the interleaving is the same
//! on every run: every later write of the device lands after the parked
//! push read its oplog and built its rows, and is pushed by a later pump.
//! (Dart's event loop could still run the next op's first statements before
//! the pump reached the wire — e.g. a partition toggled in between failed
//! the push instead of parking it. Both are legitimate schedules; this one
//! is the deterministic one a thread can give.)

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Display;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use indexmap::{IndexMap, IndexSet};
use tempfile::TempDir;
use vocara_model::models::audio_fx::TrackFx;
use vocara_model::models::audio_track::{AudioTrack, AudioTrackKind};
use vocara_model::models::fx_bus::FxBus;
use vocara_model::models::practice_note::PracticeNote;
use vocara_model::models::song_practice::SongPractice;
use vocara_model::models::take_version::TakeVersion;
use vocara_model::models::technique::Technique;
use vocara_model::models::technique_marker::TechniqueMarker;
use vocara_model::models::technique_media::{TechniqueMedia, TechniqueMediaKind};
use vocara_model::util::dart_date_time::DateTime;
use vocara_store::db::rows::{
    AssetRow, ClipRow, FxBusRow, InterpretationRow, PracticeNoteRow, PracticeRow,
    TechniqueMarkerRow, TechniqueMediaRow, TechniqueRow, TrackRow,
};
use vocara_store::db::{AppDatabase, AssetStore};
use vocara_store::repositories::{
    SongPracticeRepository, SongPracticeRepositoryDrift, TechniqueRepository,
    TechniqueRepositoryDrift,
};
use vocara_store::sync::hlc::HlcClock;
use vocara_store::sync::integrity_checker::IntegrityChecker;
use vocara_store::sync::oplog_drain::OplogDrain;
use vocara_store::sync::quarantine_store::QuarantineStore;
use vocara_store::sync::sync_applier::SyncApplier;
use vocara_store::sync::sync_engine::{
    PullRun, PushRun, SyncEngine, SyncEngineOptions, SyncEngineParts,
};
use vocara_store::sync::sync_hashes::{
    clip_row_hash, fx_bus_row_hash, interpretation_row_hash, practice_note_row_hash,
    practice_row_hash, technique_marker_row_hash, technique_media_row_hash, technique_row_hash,
    track_row_hash, Loaded,
};
use vocara_store::sync::sync_row::SyncRowBuilder;
use vocara_store::sync::sync_state_store::SyncStateStore;
use vocara_store::sync::sync_transport::{FakeCloud, FakeSyncTransport, FetchPage};
use vocara_store::sync::sync_writer::SyncWriter;
use vocara_store::Error;

use crate::dart_random::DartRandom;

/// Runs one episode of [op_count] ops from [seed] (the device count drawn
/// from the seed unless [device_count] forces it); panics with the seed,
/// the failure and the replayable op log on any convergence, invariant or
/// oracle failure.
pub fn run_sync_sim_episode(seed: i64, op_count: usize, device_count: Option<usize>) {
    let mut episode = Episode::new(seed, op_count, device_count);
    episode.run();
}

/// A fixed epoch anchor for the logical episode clock (no wall time).
fn sim_epoch_ms() -> i64 {
    DateTime::utc_ymd(2026, 1, 1).millis_since_epoch()
}

// ---------------------------------------------------------------------------
// Device stack
// ---------------------------------------------------------------------------

/// One simulated device: a full persistence stack over the shared cloud.
struct Dev {
    name: String,

    /// Fixed logical clock skew of this device (models real clock
    /// disagreement, well inside the HLC drift gate so nothing quarantines
    /// on time alone).
    skew_ms: i64,

    db: Arc<AppDatabase>,
    song_repo: SongPracticeRepositoryDrift,
    tech_repo: TechniqueRepositoryDrift,
    transport: Arc<FakeSyncTransport>,
    /// The transport's partition knob as last set (Dart read it back off the
    /// fake; the Rust fake only takes it).
    partitioned: bool,
    engine: Arc<SyncEngine>,
    state: SyncStateStore,
    quarantine: QuarantineStore,
    checker: IntegrityChecker,

    /// Open editors (0..2): each holds an aggregate + base hashes across
    /// other devices' syncs, exactly like a real open screen.
    editors: Vec<Editor>,

    /// Armed by the op mix: the next pulled page's commit hook fails once
    /// (the wave-2 killed-commit path); the retry then succeeds.
    kill_next_page_commit: Arc<AtomicBool>,

    /// A pump_push parked behind the transport's hold, if any (its thread).
    held_push: Option<JoinHandle<PushRun>>,

    // Dropped last: the pool's directory outlives every handle on it.
    _asset_root: TempDir,
}

impl Drop for Dev {
    fn drop(&mut self) {
        // Unpark anything held so no thread is left blocked on the
        // transport (the episode may be unwinding from a failure).
        self.transport.set_hold_pushes(false);
        self.transport.release_held_pushes();
        if let Some(parked) = self.held_push.take() {
            let _ = parked.join();
        }
        self.engine.dispose();
    }
}

/// The aggregate an editor holds.
#[derive(Clone)]
enum Held {
    Practice(SongPractice),
    Technique(Technique),
}

/// One open editor: the held aggregate + the base-hash map captured at
/// load, mutated in memory by edit ops and saved (possibly stale) later.
struct Editor {
    handle: String,
    held: Held,
    base: IndexMap<String, String>,

    /// Any in-memory edit since open / last successful save.
    dirty: bool,

    /// An edit that changes the ROOT row's content hash (rename etc.) — the
    /// only kind of edit that can revive an explicitly deleted root under
    /// LWW.
    root_dirty: bool,

    /// (table, id) of child rows this editor removed since the last save;
    /// the tombstones land at save time and are recorded by the oracle then.
    pending_removals: Vec<(String, String)>,
}

impl Editor {
    fn practice(handle: &str, l: Loaded<SongPractice>) -> Editor {
        Editor {
            handle: handle.to_string(),
            held: Held::Practice(l.aggregate),
            base: l.base_hashes,
            dirty: false,
            root_dirty: false,
            pending_removals: Vec::new(),
        }
    }

    fn technique(handle: &str, l: Loaded<Technique>) -> Editor {
        Editor {
            handle: handle.to_string(),
            held: Held::Technique(l.aggregate),
            base: l.base_hashes,
            dirty: false,
            root_dirty: false,
            pending_removals: Vec::new(),
        }
    }

    fn is_practice(&self) -> bool {
        matches!(self.held, Held::Practice(_))
    }
}

// ---------------------------------------------------------------------------
// Oracle
// ---------------------------------------------------------------------------

/// The harness-side model of what liveness outcomes are legitimate.
#[derive(Default)]
struct Oracle {
    /// handle -> table ('practices' | 'techniques') and row id.
    root_table: HashMap<String, &'static str>,
    root_id: HashMap<String, String>,

    /// Every root LIVE write (creation, root-dirty editor save) and every
    /// explicit delete, as (packed hlc, live). Entity-level LWW makes the
    /// max packed hlc the authoritative liveness — see
    /// [`Oracle::expected_live`]. Integrity heals are the one unrecorded
    /// root writer and they only ever run on a row that is ALREADY live, so
    /// they can never flip the expected outcome — any mismatch is a real
    /// resurrection/loss bug.
    root_writes: HashMap<String, Vec<(String, bool)>>,

    /// Roots the harness explicitly deleted at least once.
    ever_deleted: HashSet<String>,

    /// Child rows any editor content-edited or inserted at ANY point (the
    /// conservative "an active editor held an edit over it" superset): only
    /// these may legitimately be live after an explicit removal.
    live_touched: HashSet<(String, String)>,

    /// Child rows observed tombstoned right after an editor removal saved.
    explicit_child_deletes: IndexSet<(String, String)>,

    /// Every OBSERVED committed root-row state, per handle: (packed hlc,
    /// content field). The content field is the root's human-visible column
    /// (practices.title / techniques.name), read back from the row right
    /// after every creation and every successful save — each entry is a
    /// real, committed (hlc -> content) association. This is the
    /// lost-update canary: at quiescence a LIVE root must show the content
    /// of the highest-HLC observation. Derived writes (integrity heals, slot
    /// resolutions) bump a row's HLC WITHOUT changing content, so a device
    /// may hold a higher HLC than any observation — but never different
    /// content, unless a derived full-row snapshot reverted a real edit (the
    /// known derived-repair-stamp gap; see docs/sync-protocol.md, "Known
    /// design gap").
    root_content: HashMap<String, Vec<(String, String)>>,
}

impl Oracle {
    fn record_root_write(&mut self, handle: &str, hlc_packed: String, live: bool) {
        self.root_writes
            .entry(handle.to_string())
            .or_default()
            .push((hlc_packed, live));
    }

    fn record_root_content(&mut self, handle: &str, hlc_packed: String, content: String) {
        self.root_content
            .entry(handle.to_string())
            .or_default()
            .push((hlc_packed, content));
    }

    /// The content of the highest-HLC observed root state (never None for a
    /// created root — creation records the first observation).
    fn expected_content(&self, handle: &str) -> Option<&str> {
        let states = self.root_content.get(handle)?;
        let mut best = states.first()?;
        for s in &states[1..] {
            if s.0 > best.0 {
                best = s;
            }
        }
        Some(&best.1)
    }

    /// Whether [handle] should be LIVE everywhere at quiescence: the
    /// recorded write with the greatest packed HLC wins (exactly the
    /// engine's LWW rule).
    fn expected_live(&self, handle: &str) -> bool {
        let writes = &self.root_writes[handle];
        let mut best = &writes[0];
        for w in &writes[1..] {
            if w.0 > best.0 {
                best = w;
            }
        }
        best.1
    }
}

// ---------------------------------------------------------------------------
// Episode
// ---------------------------------------------------------------------------

const STEP_MS: i64 = 40;
const MAX_PRACTICES: usize = 6;
const MAX_TECHNIQUES: usize = 4;
const MAX_EDITORS_PER_DEVICE: usize = 2;

/// The op mix, by weight (one entry per unit of weight, in Dart's order —
/// the draw is an index into this list).
const WEIGHTED_OPS: [(&str, usize); 18] = [
    ("createPractice", 5),
    ("createTechnique", 3),
    ("openEditor", 12),
    ("editHeld", 26),
    ("saveHeld", 14),
    ("reloadHeld", 5),
    ("closeHeld", 4),
    ("deletePractice", 3),
    ("deleteTechnique", 2),
    ("push", 16),
    ("pull", 16),
    ("partitionToggle", 5),
    ("pageSize", 4),
    ("injectConflict", 3),
    ("redeliver", 3),
    ("killNextPage", 3),
    ("beginHeldPush", 4),
    ("releaseHeldPush", 6),
];

fn weighted_ops() -> Vec<&'static str> {
    WEIGHTED_OPS
        .iter()
        .flat_map(|&(op, weight)| std::iter::repeat_n(op, weight))
        .collect()
}

/// One applicable in-memory edit of a held practice (the closures of Dart's
/// action list; each draws its randomness only when chosen).
#[derive(Clone, Copy)]
enum PracticeEdit {
    Rename,
    MasterVolume,
    AddTake,
    TrackFader,
    RenameTrack,
    AddNote,
    AddMarker,
    RemoveTake,
    EditNote,
    RemoveNote,
    RemoveMarker,
    AddBus,
    RouteTrackToBus,
    RemoveBus,
    SetAccompaniment,
    RemoveAccompaniment,
}

/// One applicable in-memory edit of a held technique.
#[derive(Clone, Copy)]
enum TechniqueEdit {
    Rename,
    EditSummary,
    EditCues,
    AddMedia,
    EditMediaLabel,
    RemoveMedia,
}

/// Appends [what] [weight] times (Dart's `add(label, apply, weight:)`).
fn add<T: Copy>(actions: &mut Vec<(&'static str, T)>, label: &'static str, what: T, weight: usize) {
    for _ in 0..weight {
        actions.push((label, what));
    }
}

struct Episode {
    seed: i64,
    op_count: usize,
    forced_devices: Option<usize>,
    rng: DartRandom,

    cloud: Arc<FakeCloud>,
    devices: Vec<Dev>,
    oracle: Oracle,
    op_log: Vec<String>,

    /// The shared logical episode clock; each device reads it plus its
    /// skew.
    logical_ms: Arc<AtomicI64>,

    /// Monotonic counter behind every harness-minted row id.
    id_counter: usize,

    /// Registry of created roots in creation order — the seed-stable
    /// identity space (ops always pick by registry index so the episode
    /// structure is reproducible).
    practice_handles: Vec<String>,
    technique_handles: Vec<String>,

    src_files: Vec<PathBuf>,
    // Dropped after the devices (field order): no file a device imported
    // from is pulled out from under it.
    _src_dir: Option<TempDir>,

    op_index: usize,
    weighted_ops: Vec<&'static str>,
}

impl Episode {
    fn new(seed: i64, op_count: usize, forced_devices: Option<usize>) -> Episode {
        Episode {
            seed,
            op_count,
            forced_devices,
            rng: DartRandom::new(seed),
            cloud: FakeCloud::new(),
            devices: Vec::new(),
            oracle: Oracle::default(),
            op_log: Vec::new(),
            logical_ms: Arc::new(AtomicI64::new(0)),
            id_counter: 0,
            practice_handles: Vec::new(),
            technique_handles: Vec::new(),
            src_files: Vec::new(),
            _src_dir: None,
            op_index: 0,
            weighted_ops: weighted_ops(),
        }
    }

    // --- id + time helpers -------------------------------------------------

    /// Deterministic 26-char row id (the schema requires 26..36 chars).
    fn mint_id(&mut self, tag: &str) -> String {
        let n = self.id_counter;
        self.id_counter += 1;
        format!("{:0<26}", format!("SIM{tag}{n:05}"))
    }

    fn dev_now(&self, di: usize) -> i64 {
        sim_epoch_ms() + self.logical_ms.load(Ordering::SeqCst) + self.devices[di].skew_ms
    }

    fn dev_dt(&self, di: usize) -> DateTime {
        DateTime::from_millis_since_epoch(self.dev_now(di), false).expect("in range")
    }

    fn tick(&self) {
        self.logical_ms.fetch_add(STEP_MS, Ordering::SeqCst);
    }

    fn log(&mut self, line: String) {
        self.op_log.push(line);
    }

    fn name(&self, di: usize) -> String {
        self.devices[di].name.clone()
    }

    fn fail(&self, msg: &str) -> ! {
        panic!(
            "{msg}\n\nSEED: {} (0x{:x})\ndevices: {} (skews: {})\nREPLAYABLE OP LOG ({} \
             entries):\n{}",
            self.seed,
            self.seed,
            self.devices.len(),
            self.devices
                .iter()
                .map(|d| d.skew_ms.to_string())
                .collect::<Vec<_>>()
                .join(", "),
            self.op_log.len(),
            self.op_log.join("\n"),
        );
    }

    // --- setup ---------------------------------------------------------------

    fn set_up(&mut self) {
        let src_dir = tempfile::Builder::new()
            .prefix("vocara_sim_src")
            .tempdir()
            .expect("temp dir");
        // A small pool of deterministic source "audio" files; repeated
        // imports dedup by content hash, which is the production steady
        // state.
        for i in 0..3i64 {
            let f = src_dir.path().join(format!("src_{i}.wav"));
            let bytes: Vec<u8> = (0..64i64)
                .map(|j| ((self.seed + i * 131 + j * 7) & 0xff) as u8)
                .collect();
            std::fs::write(&f, bytes).expect("write source file");
            self.src_files.push(f);
        }
        self._src_dir = Some(src_dir);
        let n = self
            .forced_devices
            .unwrap_or_else(|| 2 + self.rng.next_int(3)); // 2..4 devices
        for i in 0..n {
            let skew = self.rng.next_i64(2400);
            let dev = self.build_device(i, skew);
            self.devices.push(dev);
        }
    }

    fn build_device(&self, i: usize, skew_ms: i64) -> Dev {
        // Fixed 26-char node id: the final LWW tiebreak must be seed-stable.
        let node = format!("{:0<26}", format!("SIMNODE{i}"));
        let db = Arc::new(AppDatabase::open_in_memory().expect("in-memory library"));
        let logical = self.logical_ms.clone();
        let epoch = sim_epoch_ms();
        let clock = Arc::new(
            HlcClock::new(node).with_now(move || epoch + logical.load(Ordering::SeqCst) + skew_ms),
        );
        let writer = Arc::new(SyncWriter::new(db.clone(), clock.clone()));
        let root = tempfile::Builder::new()
            .prefix(&format!("vocara_sim_d{i}"))
            .tempdir()
            .expect("temp dir");
        let assets = Arc::new(AssetStore::new(
            db.clone(),
            root.path().to_path_buf(),
            writer.clone(),
        ));
        let song_repo =
            SongPracticeRepositoryDrift::new(db.clone(), writer.clone(), assets.clone());
        let tech_repo = TechniqueRepositoryDrift::new(db.clone(), writer.clone(), assets.clone());
        let name = format!("d{i}");
        let transport = Arc::new(FakeSyncTransport::new(self.cloud.clone(), name.clone()));
        let kill = Arc::new(AtomicBool::new(false));
        // Persistence::attach_sync's wiring, with the page-commit hook.
        let builder = Arc::new(SyncRowBuilder::new(db.clone()));
        let hook_kill = kill.clone();
        let hook_name = name.clone();
        let engine = Arc::new(SyncEngine::new(
            SyncEngineParts {
                db: db.clone(),
                applier: Arc::new(SyncApplier::new(
                    db.clone(),
                    clock,
                    builder.clone(),
                    Some(QuarantineStore::new(db.clone())),
                )),
                drain: OplogDrain::new(db.clone()),
                builder,
                state: SyncStateStore::new(db.clone()),
                integrity: Arc::new(IntegrityChecker::new(db.clone(), writer.clone())),
                transport: transport.clone(),
            },
            SyncEngineOptions {
                debug_before_page_commit: Some(Arc::new(move |_: &FetchPage| {
                    if hook_kill.swap(false, Ordering::SeqCst) {
                        return Err(Error::State(format!(
                            "sim: killed page commit on {hook_name}"
                        )));
                    }
                    Ok(())
                })),
                ..SyncEngineOptions::default()
            },
        ));
        Dev {
            name,
            skew_ms,
            state: SyncStateStore::new(db.clone()),
            quarantine: QuarantineStore::new(db.clone()),
            checker: IntegrityChecker::new(db.clone(), writer),
            db,
            song_repo,
            tech_repo,
            transport,
            partitioned: false,
            engine,
            editors: Vec::new(),
            kill_next_page_commit: kill,
            held_push: None,
            _asset_root: root,
        }
    }

    // --- top level ------------------------------------------------------------

    fn run(&mut self) {
        // Teardown is Drop: every device unparks its held push, disposes its
        // engine and removes its pool, and the source files go last — on
        // success and on a failure's unwind alike.
        self.set_up();
        self.prologue();
        for _ in 0..self.op_count {
            self.op_index = self.op_log.len();
            self.tick();
            self.step();
        }
        self.quiesce();
        self.assert_converged();
        self.assert_invariants();
        self.assert_oracle();
    }

    /// Guarantees shared state early: one practice created, pushed, and
    /// pulled by everyone, so every seed exercises cross-device merging.
    fn prologue(&mut self) {
        self.tick();
        let d = self.rng.next_int(self.devices.len());
        self.create_practice(d);
        self.tick();
        self.pump(d, true);
        for other in 0..self.devices.len() {
            if other == d {
                continue;
            }
            self.tick();
            self.pump(other, false);
        }
    }

    // --- op mix -------------------------------------------------------------------

    fn step(&mut self) {
        let op = self.weighted_ops[self.rng.next_int(self.weighted_ops.len())];
        let d = self.rng.next_int(self.devices.len());
        let i = self.op_index;
        let name = self.name(d);
        match op {
            "createPractice" => {
                if self.practice_handles.len() >= MAX_PRACTICES {
                    self.log(format!("#{i} {name} createPractice -> skipped (cap)"));
                } else {
                    self.create_practice(d);
                }
            }
            "createTechnique" => {
                if self.technique_handles.len() >= MAX_TECHNIQUES {
                    self.log(format!("#{i} {name} createTechnique -> skipped (cap)"));
                } else {
                    self.create_technique(d);
                }
            }
            "openEditor" => self.open_editor(d, ""),
            "editHeld" => {
                if self.devices[d].editors.is_empty() {
                    self.open_editor(d, "edit->open");
                } else {
                    let ei = self.rng.next_int(self.devices[d].editors.len());
                    let mut e = self.devices[d].editors.remove(ei);
                    self.edit_held(d, &mut e);
                    self.devices[d].editors.insert(ei, e);
                }
            }
            "saveHeld" => {
                if self.devices[d].editors.is_empty() {
                    self.log(format!("#{i} {name} saveHeld -> skipped (no editor)"));
                } else {
                    let ei = self.rng.next_int(self.devices[d].editors.len());
                    let mut e = self.devices[d].editors.remove(ei);
                    if self.save_held(d, &mut e) {
                        self.devices[d].editors.insert(ei, e);
                    }
                }
            }
            "reloadHeld" => {
                if self.devices[d].editors.is_empty() {
                    self.log(format!("#{i} {name} reloadHeld -> skipped (no editor)"));
                } else {
                    let ei = self.rng.next_int(self.devices[d].editors.len());
                    let mut e = self.devices[d].editors.remove(ei);
                    if self.reload_held(d, &mut e) {
                        self.devices[d].editors.insert(ei, e);
                    }
                }
            }
            "closeHeld" => {
                if self.devices[d].editors.is_empty() {
                    self.log(format!("#{i} {name} closeHeld -> skipped (no editor)"));
                } else {
                    let ei = self.rng.next_int(self.devices[d].editors.len());
                    let e = self.devices[d].editors.remove(ei);
                    self.log(format!("#{i} {name} closeHeld({}) discarded", e.handle));
                }
            }
            "deletePractice" => self.delete_root(d, true),
            "deleteTechnique" => self.delete_root(d, false),
            "push" => self.pump(d, true),
            "pull" => self.pump(d, false),
            "partitionToggle" => {
                let dev = &mut self.devices[d];
                let now_partitioned = !dev.partitioned;
                dev.partitioned = now_partitioned;
                dev.transport.set_partitioned(now_partitioned);
                self.log(format!(
                    "#{i} {name} partition -> {}",
                    if now_partitioned {
                        "OFF-LINE"
                    } else {
                        "online"
                    }
                ));
            }
            "pageSize" => {
                let size = 1 + self.rng.next_int(5);
                self.devices[d].transport.set_page_size(size);
                self.log(format!("#{i} {name} pageSize={size}"));
            }
            "injectConflict" => self.inject_conflict(d),
            "redeliver" => {
                let n = 1 + self.rng.next_int(2);
                self.devices[d].transport.set_redeliver_next_fetches(n);
                self.log(format!("#{i} {name} redeliverNextFetches={n}"));
            }
            "killNextPage" => {
                self.devices[d]
                    .kill_next_page_commit
                    .store(true, Ordering::SeqCst);
                self.log(format!("#{i} {name} armed killNextPageCommit"));
            }
            "beginHeldPush" => self.begin_held_push(d),
            "releaseHeldPush" => {
                if self.devices[d].held_push.is_none() {
                    self.log(format!("#{i} {name} releaseHeldPush -> skipped (none)"));
                } else {
                    self.release_held(d);
                }
            }
            other => unreachable!("unknown op {other}"),
        }
    }

    // --- creation ---------------------------------------------------------------

    fn create_practice(&mut self, d: usize) {
        let handle = format!("P{}", self.practice_handles.len());
        let id = self.mint_id("PR");
        let version = TakeVersion::new(self.mint_id("IV"), "Interpretation 1", self.dev_dt(d));
        let practice = SongPractice {
            active_version_id: Some(version.id.clone()),
            versions: vec![Arc::new(version)],
            ..SongPractice::new(
                id.clone(),
                format!("{handle} v0"),
                self.dev_dt(d),
                self.dev_dt(d),
            )
        };
        // Creation flow: the base-less legacy save is the documented path
        // for a fresh aggregate (see save_practice docs / the round-trip
        // suite).
        if let Err(e) = self.devices[d].song_repo.save_practice(&practice) {
            self.fail(&format!(
                "#{} {} createPractice({handle}) failed: {e}",
                self.op_index,
                self.name(d)
            ));
        }
        self.practice_handles.push(handle.clone());
        self.oracle.root_table.insert(handle.clone(), "practices");
        self.oracle.root_id.insert(handle.clone(), id.clone());
        let hlc = self.root_hlc(d, "practices", &id);
        self.oracle.record_root_write(&handle, hlc, true);
        self.observe_root_content(d, &handle);
        let line = format!(
            "#{} {} createPractice({handle})",
            self.op_index,
            self.name(d)
        );
        self.log(line);
    }

    fn create_technique(&mut self, d: usize) {
        let handle = format!("T{}", self.technique_handles.len());
        let id = self.mint_id("TQ");
        let technique = Technique {
            summary: "summary v0".to_string(),
            ..Technique::new(
                id.clone(),
                format!("{handle} v0"),
                self.dev_dt(d),
                self.dev_dt(d),
            )
        };
        if let Err(e) = self.devices[d].tech_repo.save_technique(&technique) {
            self.fail(&format!(
                "#{} {} createTechnique({handle}) failed: {e}",
                self.op_index,
                self.name(d)
            ));
        }
        self.technique_handles.push(handle.clone());
        self.oracle.root_table.insert(handle.clone(), "techniques");
        self.oracle.root_id.insert(handle.clone(), id.clone());
        let hlc = self.root_hlc(d, "techniques", &id);
        self.oracle.record_root_write(&handle, hlc, true);
        self.observe_root_content(d, &handle);
        let line = format!(
            "#{} {} createTechnique({handle})",
            self.op_index,
            self.name(d)
        );
        self.log(line);
    }

    // --- editor lifecycle -----------------------------------------------------------

    /// Roots of the table that are LIVE on [d] right now, in registry order.
    fn live_handles_on(&self, d: usize, practice: bool) -> Vec<String> {
        let handles = if practice {
            &self.practice_handles
        } else {
            &self.technique_handles
        };
        handles
            .iter()
            .filter(|h| {
                is_live(
                    &self.devices[d].db,
                    self.oracle.root_table[*h],
                    &self.oracle.root_id[*h],
                )
            })
            .cloned()
            .collect()
    }

    fn root_hlc(&self, d: usize, table: &str, id: &str) -> String {
        match self.devices[d]
            .db
            .query_opt(
                &format!("SELECT hlc_packed AS h FROM {table} WHERE id = ?"),
                [id],
                |r| r.get::<_, String>(0),
            )
            .expect("query")
        {
            Some(h) => h,
            None => self.fail(&format!(
                "#{} {} expected a {table} row for {id}",
                self.op_index,
                self.name(d)
            )),
        }
    }

    /// Records an oracle content observation of [handle]'s committed root
    /// row on [d]: the (hlc_packed, title/name) pair exactly as persisted
    /// right now. Called after every creation and every successful save, so
    /// the oracle knows which content belongs to which HLC (see
    /// [`Oracle::root_content`]).
    fn observe_root_content(&mut self, d: usize, handle: &str) {
        let table = self.oracle.root_table[handle];
        let id = &self.oracle.root_id[handle];
        let col = if table == "practices" {
            "title"
        } else {
            "name"
        };
        let row = self.devices[d]
            .db
            .query_opt(
                &format!("SELECT hlc_packed AS h, {col} AS c FROM {table} WHERE id = ?"),
                [id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .expect("query");
        if let Some((h, c)) = row {
            self.oracle.record_root_content(handle, h, c);
        }
    }

    fn open_editor(&mut self, d: usize, note: &str) {
        let i = self.op_index;
        let name = self.name(d);
        if self.devices[d].editors.len() >= MAX_EDITORS_PER_DEVICE {
            self.log(format!(
                "#{i} {name} openEditor -> skipped (2 already open)"
            ));
            return;
        }
        let want_practice = self.technique_handles.is_empty() || self.rng.next_int(10) < 7;
        let candidates = self.live_handles_on(d, want_practice);
        if candidates.is_empty() {
            self.log(format!("#{i} {name} openEditor -> skipped (nothing live)"));
            return;
        }
        let handle = candidates[self.rng.next_int(candidates.len())].clone();
        let id = self.oracle.root_id[&handle].clone();
        let editor = if want_practice {
            match self.devices[d].song_repo.load_practice(&id) {
                Ok(l) => Editor::practice(&handle, l),
                Err(_) => {
                    self.log(format!("#{i} {name} openEditor({handle}) -> load failed"));
                    return;
                }
            }
        } else {
            match self.devices[d].tech_repo.load_technique(&id) {
                Ok(l) => Editor::technique(&handle, l),
                Err(_) => {
                    self.log(format!("#{i} {name} openEditor({handle}) -> load failed"));
                    return;
                }
            }
        };
        self.devices[d].editors.push(editor);
        self.log(format!("#{i} {name} openEditor({handle}){note}"));
    }

    /// Reloads [e] from the store; false when its root is gone (the editor
    /// is closed).
    fn reload_held(&mut self, d: usize, e: &mut Editor) -> bool {
        let i = self.op_index;
        let name = self.name(d);
        let id = self.oracle.root_id[&e.handle].clone();
        let reloaded = if e.is_practice() {
            self.devices[d]
                .song_repo
                .load_practice(&id)
                .map(|l| (Held::Practice(l.aggregate), l.base_hashes))
        } else {
            self.devices[d]
                .tech_repo
                .load_technique(&id)
                .map(|l| (Held::Technique(l.aggregate), l.base_hashes))
        };
        match reloaded {
            Ok((held, base)) => {
                e.held = held;
                e.base = base;
                e.dirty = false;
                e.root_dirty = false;
                e.pending_removals.clear();
                self.log(format!("#{i} {name} reload({})", e.handle));
                true
            }
            Err(_) => {
                self.log(format!(
                    "#{i} {name} reload({}) -> gone, editor closed",
                    e.handle
                ));
                false
            }
        }
    }

    // --- in-memory edits -----------------------------------------------------------

    fn edit_held(&mut self, d: usize, e: &mut Editor) {
        if e.is_practice() {
            self.edit_practice(d, e);
        } else {
            self.edit_technique(d, e);
        }
    }

    fn touch(&mut self, table: &str, id: &str) {
        self.oracle
            .live_touched
            .insert((table.to_string(), id.to_string()));
    }

    fn edit_practice(&mut self, d: usize, e: &mut Editor) {
        let Held::Practice(p) = e.held.clone() else {
            unreachable!("a practice editor")
        };
        let vocals: Vec<Arc<AudioTrack>> = p.vocal_tracks.clone();
        let op = self.op_index;
        // Weighted list of applicable edit actions (label, edit).
        let mut actions: Vec<(&'static str, PracticeEdit)> = Vec::new();
        add(&mut actions, "rename", PracticeEdit::Rename, 3);
        add(&mut actions, "masterVolume", PracticeEdit::MasterVolume, 1);
        add(&mut actions, "addTake", PracticeEdit::AddTake, 2);
        let with_notes: Vec<Arc<AudioTrack>> = vocals
            .iter()
            .filter(|t| !t.notes.is_empty())
            .cloned()
            .collect();
        let with_markers: Vec<Arc<AudioTrack>> = vocals
            .iter()
            .filter(|t| !t.technique_markers.is_empty())
            .cloned()
            .collect();
        if !vocals.is_empty() {
            add(&mut actions, "trackFader", PracticeEdit::TrackFader, 4);
            add(&mut actions, "renameTrack", PracticeEdit::RenameTrack, 1);
            add(&mut actions, "addNote", PracticeEdit::AddNote, 3);
            add(&mut actions, "addMarker", PracticeEdit::AddMarker, 2);
            add(&mut actions, "removeTake", PracticeEdit::RemoveTake, 1);
            if !with_notes.is_empty() {
                add(&mut actions, "editNote", PracticeEdit::EditNote, 3);
                add(&mut actions, "removeNote", PracticeEdit::RemoveNote, 1);
            }
            if !with_markers.is_empty() {
                add(&mut actions, "removeMarker", PracticeEdit::RemoveMarker, 1);
            }
        }
        add(&mut actions, "addBus", PracticeEdit::AddBus, 1);
        if !p.fx_buses.is_empty() {
            if !vocals.is_empty() {
                add(
                    &mut actions,
                    "routeTrackToBus",
                    PracticeEdit::RouteTrackToBus,
                    2,
                );
            }
            add(&mut actions, "removeBus", PracticeEdit::RemoveBus, 1);
        }
        if p.accompaniment.is_none() {
            add(
                &mut actions,
                "setAccompaniment",
                PracticeEdit::SetAccompaniment,
                1,
            );
        } else {
            add(
                &mut actions,
                "removeAccompaniment",
                PracticeEdit::RemoveAccompaniment,
                1,
            );
        }

        let (label, edit) = actions[self.rng.next_int(actions.len())];
        // The index of [t] in the take list (Dart's `vocals.indexOf(t)`).
        let index_of = |t: &Arc<AudioTrack>| {
            vocals
                .iter()
                .position(|v| Arc::ptr_eq(v, t))
                .expect("a listed take")
        };
        let next = match edit {
            PracticeEdit::Rename => {
                e.root_dirty = true;
                SongPractice {
                    title: format!("{} v{op}", e.handle),
                    ..p
                }
            }
            PracticeEdit::MasterVolume => {
                let v = (1 + self.rng.next_i64(10)) as f64 / 10.0;
                if v != p.master_volume {
                    e.root_dirty = true;
                }
                SongPractice {
                    master_volume: v,
                    ..p
                }
            }
            PracticeEdit::AddTake => {
                let id = self.mint_id("TK");
                let file = self.src_files[self.rng.next_int(self.src_files.len())].clone();
                self.touch("tracks", &id);
                self.touch("clips", &format!("{id}-c0"));
                let duration_ms = 800 + self.rng.next_i64(2000);
                let take = AudioTrack {
                    version_id: p.active_version_id.clone(),
                    ..AudioTrack::new(
                        id,
                        AudioTrackKind::Vocal,
                        format!("take v{op}"),
                        self.dev_dt(d),
                    )
                    .with_audio(
                        file.to_string_lossy(),
                        0,
                        duration_ms,
                        Vec::new(),
                    )
                };
                let mut tracks = vocals.clone();
                tracks.push(Arc::new(take));
                SongPractice {
                    vocal_tracks: tracks,
                    ..p
                }
            }
            PracticeEdit::TrackFader => {
                let i = self.rng.next_int(vocals.len());
                let t = vocals[i].clone();
                self.touch("tracks", &t.id);
                let volume = (1 + self.rng.next_i64(10)) as f64 / 10.0;
                let pan = (self.rng.next_i64(9) - 4) as f64 / 4.0;
                let mut tracks = vocals.clone();
                tracks[i] = Arc::new(AudioTrack {
                    volume,
                    pan,
                    ..(*t).clone()
                });
                SongPractice {
                    vocal_tracks: tracks,
                    ..p
                }
            }
            PracticeEdit::RenameTrack => {
                let i = self.rng.next_int(vocals.len());
                self.touch("tracks", &vocals[i].id);
                let mut tracks = vocals.clone();
                tracks[i] = Arc::new(AudioTrack {
                    display_name: format!("take v{op}"),
                    ..(*vocals[i]).clone()
                });
                SongPractice {
                    vocal_tracks: tracks,
                    ..p
                }
            }
            PracticeEdit::AddNote => {
                let i = self.rng.next_int(vocals.len());
                let t = vocals[i].clone();
                let id = self.mint_id("NT");
                self.touch("practice_notes", &id);
                let note = PracticeNote {
                    id,
                    position_ms: self.rng.next_i64(30) * 100,
                    end_ms: None,
                    content: format!("note v{op}"),
                    created_at: self.dev_dt(d),
                    updated_at: self.dev_dt(d),
                };
                let mut notes = t.notes.clone();
                notes.push(note);
                let mut tracks = vocals.clone();
                tracks[i] = Arc::new(AudioTrack {
                    notes,
                    ..(*t).clone()
                });
                SongPractice {
                    vocal_tracks: tracks,
                    ..p
                }
            }
            PracticeEdit::AddMarker => {
                let i = self.rng.next_int(vocals.len());
                let t = vocals[i].clone();
                let id = self.mint_id("MK");
                self.touch("technique_markers", &id);
                // Markers reference techniques by SOFT id + name snapshot by
                // design.
                let tech_name = if self.technique_handles.is_empty() {
                    "freehand".to_string()
                } else {
                    self.technique_handles[self.rng.next_int(self.technique_handles.len())].clone()
                };
                let position_ms = self.rng.next_i64(30) * 100;
                let technique_id = if self.technique_handles.is_empty() {
                    self.mint_id("XX")
                } else {
                    self.oracle.root_id[&tech_name].clone()
                };
                let marker = TechniqueMarker {
                    id,
                    position_ms,
                    end_ms: None,
                    technique_id,
                    technique_name: tech_name,
                    created_at: self.dev_dt(d),
                    updated_at: self.dev_dt(d),
                };
                let mut markers = t.technique_markers.clone();
                markers.push(marker);
                let mut tracks = vocals.clone();
                tracks[i] = Arc::new(AudioTrack {
                    technique_markers: markers,
                    ..(*t).clone()
                });
                SongPractice {
                    vocal_tracks: tracks,
                    ..p
                }
            }
            PracticeEdit::RemoveTake => {
                let i = self.rng.next_int(vocals.len());
                let t = &vocals[i];
                e.pending_removals
                    .push(("tracks".to_string(), t.id.clone()));
                for c in &t.clips {
                    e.pending_removals.push(("clips".to_string(), c.id.clone()));
                }
                for n in &t.notes {
                    e.pending_removals
                        .push(("practice_notes".to_string(), n.id.clone()));
                }
                for m in &t.technique_markers {
                    e.pending_removals
                        .push(("technique_markers".to_string(), m.id.clone()));
                }
                let mut tracks = vocals.clone();
                tracks.remove(i);
                SongPractice {
                    vocal_tracks: tracks,
                    ..p
                }
            }
            PracticeEdit::EditNote => {
                let t = with_notes[self.rng.next_int(with_notes.len())].clone();
                let ni = self.rng.next_int(t.notes.len());
                self.touch("practice_notes", &t.notes[ni].id);
                let mut notes = t.notes.clone();
                notes[ni] = PracticeNote {
                    content: format!("note v{op} edited"),
                    updated_at: self.dev_dt(d),
                    ..t.notes[ni].clone()
                };
                let ti = index_of(&t);
                let mut tracks = vocals.clone();
                tracks[ti] = Arc::new(AudioTrack {
                    notes,
                    ..(*t).clone()
                });
                SongPractice {
                    vocal_tracks: tracks,
                    ..p
                }
            }
            PracticeEdit::RemoveNote => {
                let t = with_notes[self.rng.next_int(with_notes.len())].clone();
                let ni = self.rng.next_int(t.notes.len());
                e.pending_removals
                    .push(("practice_notes".to_string(), t.notes[ni].id.clone()));
                let ti = index_of(&t);
                let mut notes = t.notes.clone();
                notes.remove(ni);
                let mut tracks = vocals.clone();
                tracks[ti] = Arc::new(AudioTrack {
                    notes,
                    ..(*t).clone()
                });
                SongPractice {
                    vocal_tracks: tracks,
                    ..p
                }
            }
            PracticeEdit::RemoveMarker => {
                let t = with_markers[self.rng.next_int(with_markers.len())].clone();
                let mi = self.rng.next_int(t.technique_markers.len());
                e.pending_removals.push((
                    "technique_markers".to_string(),
                    t.technique_markers[mi].id.clone(),
                ));
                let ti = index_of(&t);
                let mut markers = t.technique_markers.clone();
                markers.remove(mi);
                let mut tracks = vocals.clone();
                tracks[ti] = Arc::new(AudioTrack {
                    technique_markers: markers,
                    ..(*t).clone()
                });
                SongPractice {
                    vocal_tracks: tracks,
                    ..p
                }
            }
            PracticeEdit::AddBus => {
                let id = self.mint_id("BU");
                self.touch("fx_buses", &id);
                let bus = FxBus {
                    volume: (1 + self.rng.next_i64(10)) as f64 / 10.0,
                    ..FxBus::new(id, format!("bus v{op}"), TrackFx::flat())
                };
                let mut buses = p.fx_buses.clone();
                buses.push(Arc::new(bus));
                SongPractice {
                    fx_buses: buses,
                    ..p
                }
            }
            PracticeEdit::RouteTrackToBus => {
                let ti = self.rng.next_int(vocals.len());
                let t = vocals[ti].clone();
                self.touch("tracks", &t.id);
                let unroute = self.rng.next_int(4) == 0;
                let bus_id = if unroute {
                    None
                } else {
                    Some(p.fx_buses[self.rng.next_int(p.fx_buses.len())].id.clone())
                };
                let mut tracks = vocals.clone();
                tracks[ti] = Arc::new(AudioTrack {
                    bus_id,
                    ..(*t).clone()
                });
                SongPractice {
                    vocal_tracks: tracks,
                    ..p
                }
            }
            PracticeEdit::RemoveBus => {
                let bi = self.rng.next_int(p.fx_buses.len());
                let bus = p.fx_buses[bi].clone();
                e.pending_removals
                    .push(("fx_buses".to_string(), bus.id.clone()));
                // A realistic editor clears the routing of tracks feeding the
                // removed bus in the same save (the mixer UI does exactly
                // that).
                let next_vocals: Vec<Arc<AudioTrack>> = vocals
                    .iter()
                    .map(|t| {
                        if t.bus_id.as_deref() == Some(bus.id.as_str()) {
                            Arc::new(AudioTrack {
                                bus_id: None,
                                ..(**t).clone()
                            })
                        } else {
                            t.clone()
                        }
                    })
                    .collect();
                for t in &vocals {
                    if t.bus_id.as_deref() == Some(bus.id.as_str()) {
                        self.touch("tracks", &t.id);
                    }
                }
                let mut buses = p.fx_buses.clone();
                buses.remove(bi);
                SongPractice {
                    fx_buses: buses,
                    vocal_tracks: next_vocals,
                    ..p
                }
            }
            PracticeEdit::SetAccompaniment => {
                let id = self.mint_id("AC");
                self.touch("tracks", &id);
                self.touch("clips", &format!("{id}-c0"));
                let file = self.src_files[self.rng.next_int(self.src_files.len())].clone();
                let acc = AudioTrack::new(
                    id,
                    AudioTrackKind::Accompaniment,
                    format!("accompaniment v{op}"),
                    self.dev_dt(d),
                )
                .with_audio(file.to_string_lossy(), 0, 2000, Vec::new());
                SongPractice {
                    accompaniment: Some(Arc::new(acc)),
                    ..p
                }
            }
            PracticeEdit::RemoveAccompaniment => {
                let acc = p.accompaniment.clone().expect("an accompaniment");
                e.pending_removals
                    .push(("tracks".to_string(), acc.id.clone()));
                for c in &acc.clips {
                    e.pending_removals.push(("clips".to_string(), c.id.clone()));
                }
                SongPractice {
                    accompaniment: None,
                    ..p
                }
            }
        };
        e.held = Held::Practice(next);
        e.dirty = true;
        let line = format!("#{op} {} edit({}) {label}", self.name(d), e.handle);
        self.log(line);
    }

    fn edit_technique(&mut self, d: usize, e: &mut Editor) {
        let Held::Technique(t) = e.held.clone() else {
            unreachable!("a technique editor")
        };
        let op = self.op_index;
        let mut actions: Vec<(&'static str, TechniqueEdit)> = Vec::new();
        add(&mut actions, "rename", TechniqueEdit::Rename, 2);
        add(&mut actions, "editSummary", TechniqueEdit::EditSummary, 2);
        add(&mut actions, "editCues", TechniqueEdit::EditCues, 1);
        add(&mut actions, "addMedia", TechniqueEdit::AddMedia, 1);
        if !t.media.is_empty() {
            add(
                &mut actions,
                "editMediaLabel",
                TechniqueEdit::EditMediaLabel,
                1,
            );
            add(&mut actions, "removeMedia", TechniqueEdit::RemoveMedia, 1);
        }

        let (label, edit) = actions[self.rng.next_int(actions.len())];
        let next = match edit {
            TechniqueEdit::Rename => {
                e.root_dirty = true;
                Technique {
                    name: format!("{} v{op}", e.handle),
                    ..t
                }
            }
            TechniqueEdit::EditSummary => {
                e.root_dirty = true;
                Technique {
                    summary: format!("summary v{op}"),
                    ..t
                }
            }
            TechniqueEdit::EditCues => {
                e.root_dirty = true;
                Technique {
                    cues: format!("cues v{op}"),
                    ..t
                }
            }
            TechniqueEdit::AddMedia => {
                let id = self.mint_id("ME");
                self.touch("technique_media", &id);
                let media = TechniqueMedia {
                    id,
                    path: self.src_files[self.rng.next_int(self.src_files.len())]
                        .to_string_lossy()
                        .into_owned(),
                    kind: TechniqueMediaKind::Audio,
                    label: format!("ex v{op}"),
                    added_at: self.dev_dt(d),
                };
                let mut all = t.media.clone();
                all.push(media);
                Technique { media: all, ..t }
            }
            TechniqueEdit::EditMediaLabel => {
                let mi = self.rng.next_int(t.media.len());
                self.touch("technique_media", &t.media[mi].id);
                let mut all = t.media.clone();
                all[mi] = TechniqueMedia {
                    label: format!("ex v{op} edited"),
                    ..t.media[mi].clone()
                };
                Technique { media: all, ..t }
            }
            TechniqueEdit::RemoveMedia => {
                let mi = self.rng.next_int(t.media.len());
                e.pending_removals
                    .push(("technique_media".to_string(), t.media[mi].id.clone()));
                let mut all = t.media.clone();
                all.remove(mi);
                Technique { media: all, ..t }
            }
        };
        e.held = Held::Technique(next);
        e.dirty = true;
        let line = format!("#{op} {} edit({}) {label}", self.name(d), e.handle);
        self.log(line);
    }

    // --- saves / deletes ---------------------------------------------------------------

    /// Saves [e] through the three-way path; false when the save failed (a
    /// legitimate outcome, e.g. the root was tombstoned under the editor:
    /// the UI reverts, the editor closes).
    fn save_held(&mut self, d: usize, e: &mut Editor) -> bool {
        let i = self.op_index;
        let name = self.name(d);
        let root_dirty_at_save = e.root_dirty;
        let removals = e.pending_removals.clone();
        let error: Option<Error> = match &e.held {
            Held::Practice(p) => match self.devices[d]
                .song_repo
                .save_practice_with_base(p, &e.base)
            {
                Ok(l) => {
                    e.held = Held::Practice(l.aggregate);
                    e.base = l.base_hashes;
                    None
                }
                Err(err) => Some(err),
            },
            Held::Technique(t) => {
                match self.devices[d]
                    .tech_repo
                    .save_technique_with_base(t, &e.base)
                {
                    Ok(l) => {
                        e.held = Held::Technique(l.aggregate);
                        e.base = l.base_hashes;
                        None
                    }
                    Err(err) => Some(err),
                }
            }
        };
        if let Some(error) = error {
            self.log(format!(
                "#{i} {name} save({}) FAILED ({}), editor closed",
                e.handle,
                first_line(&error)
            ));
            return false;
        }
        e.dirty = false;
        e.root_dirty = false;
        e.pending_removals.clear();
        // Oracle: a root-dirty save that committed holds live root content at
        // the row's (possibly fresh) HLC; record it as a live write.
        let table = self.oracle.root_table[&e.handle];
        let id = self.oracle.root_id[&e.handle].clone();
        if root_dirty_at_save {
            let hlc = self.root_hlc(d, table, &id);
            self.oracle.record_root_write(&e.handle, hlc, true);
        }
        // Content observation for EVERY successful save (not just root-dirty
        // ones): a save whose reconciled root content changed for any reason
        // — e.g. a child edit moving the timeline duration — rewrites the
        // root row with a fresh HLC while keeping whatever title this device
        // held, and that state can legitimately win fleet-wide LWW.
        // Observing it keeps the content oracle exactly as permissive as the
        // engine's real semantics.
        self.observe_root_content(d, &e.handle);
        // Oracle: removals that actually tombstoned a persisted row are
        // explicit deletes; if such a row is live at quiescence, some editor
        // must have touched it (rule 3/5: active editor wins) — anything
        // else is a resurrection bug.
        for (child_table, child_id) in &removals {
            if deleted_at(&self.devices[d].db, child_table, child_id).is_some_and(|x| x.is_some()) {
                self.oracle
                    .explicit_child_deletes
                    .insert((child_table.clone(), child_id.clone()));
            }
        }
        self.log(format!(
            "#{i} {name} save({}){}{}",
            e.handle,
            if root_dirty_at_save { " rootDirty" } else { "" },
            if removals.is_empty() {
                String::new()
            } else {
                format!(" removals:{}", removals.len())
            }
        ));
        true
    }

    fn delete_root(&mut self, d: usize, practice: bool) {
        let i = self.op_index;
        let name = self.name(d);
        let candidates = self.live_handles_on(d, practice);
        if candidates.is_empty() {
            self.log(format!(
                "#{i} {name} delete{} -> skipped (nothing live)",
                if practice { "Practice" } else { "Technique" }
            ));
            return;
        }
        let handle = candidates[self.rng.next_int(candidates.len())].clone();
        let table = self.oracle.root_table[&handle];
        let id = self.oracle.root_id[&handle].clone();
        let res = if practice {
            self.devices[d].song_repo.delete_practice(&id)
        } else {
            self.devices[d].tech_repo.delete_technique(&id)
        };
        if let Err(e) = res {
            self.fail(&format!("#{i} {name} delete({handle}) failed: {e}"));
        }
        self.oracle.ever_deleted.insert(handle.clone());
        let hlc = self.root_hlc(d, table, &id);
        self.oracle.record_root_write(&handle, hlc, false);
        // Editors over the deleted root on THIS device would fail their next
        // save anyway; leave them open — that is exactly the stale-editor
        // case.
        self.log(format!("#{i} {name} delete({handle})"));
    }

    // --- transport ---------------------------------------------------------------------

    fn pump(&mut self, d: usize, push: bool) {
        // Never queue behind a parked push (the engine serializes push and
        // pull): release it first, exactly like delivery completing before
        // the next op.
        if self.devices[d].held_push.is_some() {
            self.release_held(d);
        }
        let i = self.op_index;
        let name = self.name(d);
        if push {
            let run = self.devices[d].engine.pump_push();
            self.log(format!("#{i} {name} push -> {}", describe_push(&run)));
        } else {
            let run = self.devices[d].engine.pump_pull();
            self.log(format!("#{i} {name} pull -> {}", describe_pull(&run)));
        }
    }

    fn begin_held_push(&mut self, d: usize) {
        let i = self.op_index;
        let name = self.name(d);
        if self.devices[d].held_push.is_some() {
            self.log(format!(
                "#{i} {name} beginHeldPush -> skipped (already held)"
            ));
            return;
        }
        let dev = &mut self.devices[d];
        dev.transport.set_hold_pushes(true);
        let engine = dev.engine.clone();
        let parked = std::thread::Builder::new()
            .name(format!("sim-held-push-{name}"))
            .spawn(move || engine.pump_push())
            .expect("spawn the held push");
        // Run the pump up to the wire (or to its end, when it has nothing to
        // push or fails first) before the episode moves on: the Dart future
        // had drained the oplog before the next op's writes too.
        while dev.transport.held_push_count() == 0 && !parked.is_finished() {
            std::thread::yield_now();
        }
        dev.held_push = Some(parked);
        self.log(format!("#{i} {name} beginHeldPush (pump parked)"));
    }

    fn release_held(&mut self, d: usize) {
        let i = self.op_index;
        let name = self.name(d);
        let dev = &mut self.devices[d];
        let Some(parked) = dev.held_push.take() else {
            return;
        };
        // Drop the hold BEFORE releasing so follow-up chunks of the same pump
        // flow through instead of parking again.
        dev.transport.set_hold_pushes(false);
        dev.transport.release_held_pushes();
        let run = parked.join().expect("the held push");
        self.log(format!(
            "#{i} {name} releaseHeldPush -> {}",
            describe_push(&run)
        ));
    }

    fn inject_conflict(&mut self, d: usize) {
        let i = self.op_index;
        let name = self.name(d);
        let all: Vec<String> = self
            .practice_handles
            .iter()
            .chain(&self.technique_handles)
            .cloned()
            .collect();
        if all.is_empty() {
            self.log(format!("#{i} {name} injectConflict -> skipped (no roots)"));
            return;
        }
        let handle = all[self.rng.next_int(all.len())].clone();
        let times = 1 + self.rng.next_int(2);
        self.devices[d].transport.inject_conflict_once(
            self.oracle.root_table[&handle],
            &self.oracle.root_id[&handle],
            times,
        );
        self.log(format!("#{i} {name} injectConflict({handle} x{times})"));
    }

    // --- quiescence ----------------------------------------------------------------------

    fn quiesce(&mut self) {
        // Close every held editor: save or discard per seed (a failing save
        // is a discard). This drains editor intent into the store before
        // settling.
        for d in 0..self.devices.len() {
            let editors = std::mem::take(&mut self.devices[d].editors);
            for mut e in editors {
                self.op_index = self.op_log.len();
                self.tick();
                if e.dirty && self.rng.next_int(10) < 7 {
                    self.save_held(d, &mut e);
                } else {
                    let line = format!(
                        "#{} {} close({}) discarded at quiesce",
                        self.op_index,
                        self.name(d),
                        e.handle
                    );
                    self.log(line);
                }
            }
        }
        // Clear every fault so the settle loop can actually converge.
        for d in 0..self.devices.len() {
            let dev = &mut self.devices[d];
            dev.partitioned = false;
            dev.transport.set_partitioned(false);
            dev.kill_next_page_commit.store(false, Ordering::SeqCst);
            dev.transport.set_redeliver_next_fetches(0);
            self.release_held(d);
        }
        // Settle: pump everyone until no device has pending oplog entries and
        // every change token sits at the cloud head. Non-termination is a
        // bug.
        const MAX_ROUNDS: usize = 40;
        for round in 0..MAX_ROUNDS {
            self.op_index = self.op_log.len();
            self.tick();
            for dev in &self.devices {
                let run = dev.engine.pump_push();
                if let Some(f) = &run.failure {
                    self.fail(&format!(
                        "settle round {round}: {} push failed: {f}",
                        dev.name
                    ));
                }
            }
            for dev in &self.devices {
                let run = dev.engine.pump_pull();
                if let Some(f) = &run.failure {
                    self.fail(&format!(
                        "settle round {round}: {} pull failed: {f}",
                        dev.name
                    ));
                }
            }
            let head = self.cloud.latest_seq().to_string();
            let stable = self.devices.iter().all(|dev| {
                oplog_len(&dev.db) == 0
                    && dev.state.change_token().expect("change token").as_deref()
                        == Some(head.as_str())
                    && dev.transport.held_push_count() == 0
            });
            if stable {
                let line = format!(
                    "settled after {} rounds (cloud rows: {})",
                    round + 1,
                    self.cloud.latest_seq()
                );
                self.log(line);
                return;
            }
        }
        self.fail(&format!(
            "did not settle within {MAX_ROUNDS} rounds — sync is not quiescing \
             (non-termination bug)"
        ));
    }

    // --- assertions -------------------------------------------------------------------------

    fn assert_converged(&self) {
        let reference = snapshot(&self.devices[0].db);
        for d in &self.devices[1..] {
            let other = snapshot(&d.db);
            if reference == other {
                continue;
            }
            // Find and report the FIRST diverging table/id (sorted for
            // stability), with enough context to tell push-side loss from
            // apply-side loss.
            let keys: std::collections::BTreeSet<&String> =
                reference.keys().chain(other.keys()).collect();
            let diverging: Vec<&String> = keys
                .into_iter()
                .filter(|k| reference.get(*k) != other.get(*k))
                .collect();
            let k = diverging[0];
            let (table, id) = k.split_once('/').expect("table/id");
            let cloud_row = self.cloud.server_row_for(table, id);
            let mut quarantined = Vec::new();
            for dev in &self.devices {
                for q in dev.quarantine.list().expect("quarantine") {
                    if q.tbl == table && q.id == id {
                        quarantined.push(format!("{}: {}", dev.name, q.reason));
                    }
                }
            }
            // For the cloud's copy of the row, resolve every FK-ish field
            // against both devices — tells an FK-sweep drop from a plain LWW
            // mixup at a glance.
            let mut fk_notes = Vec::new();
            if let Some(cloud_row) = &cloud_row {
                for fk in [
                    "practice_id",
                    "track_id",
                    "technique_id",
                    "asset_id",
                    "interpretation_id",
                    "bus_id",
                ] {
                    let Some(v) = cloud_row.field(fk).and_then(|v| v.as_str()) else {
                        continue;
                    };
                    let parent_table = match fk {
                        "practice_id" => "practices",
                        "track_id" => "tracks",
                        "technique_id" => "techniques",
                        "asset_id" => "assets",
                        "interpretation_id" => "interpretations",
                        _ => "fx_buses",
                    };
                    let states: Vec<String> = [&self.devices[0], d]
                        .iter()
                        .map(|dev| {
                            let state = match deleted_at(&dev.db, parent_table, v) {
                                None => "missing",
                                Some(None) => "live",
                                Some(Some(_)) => "dead",
                            };
                            format!("{}:{state}", dev.name)
                        })
                        .collect();
                    fk_notes.push(format!("{fk}={v} ({})", states.join(" ")));
                }
            }
            let shown = |m: &BTreeMap<String, String>| {
                m.get(k).cloned().unwrap_or_else(|| "<missing>".to_string())
            };
            let cloud_text = match &cloud_row {
                None => "<never accepted>".to_string(),
                Some(r) => format!(
                    "{}|{}",
                    r.hlc_packed().unwrap_or("?"),
                    r.field("deleted_at")
                        .map_or("null".to_string(), |v| v.dart_to_string())
                ),
            };
            self.fail(&format!(
                "CONVERGENCE FAILURE: first diverging row: {k}\n  {}: {}\n  {}: {}\n  cloud: \
                 {cloud_text}\n  fk targets: {}\n  quarantined: {}\n  ({} diverging rows \
                 total: {}{})",
                self.devices[0].name,
                shown(&reference),
                d.name,
                shown(&other),
                if fk_notes.is_empty() {
                    "n/a".to_string()
                } else {
                    fk_notes.join("; ")
                },
                if quarantined.is_empty() {
                    "nowhere".to_string()
                } else {
                    quarantined.join("; ")
                },
                diverging.len(),
                diverging
                    .iter()
                    .take(8)
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                if diverging.len() > 8 { ", …" } else { "" },
            ));
        }
    }

    fn assert_invariants(&self) {
        let head = self.cloud.latest_seq().to_string();
        for d in &self.devices {
            // 1. Everything the IntegrityChecker claims to keep must actually
            //    hold: no live child of a tombstoned parent, no dangling
            //    scalar pointer (active_interpretation_id / bus_id /
            //    interpretation_id / output_bus_id / separation_asset_id), no
            //    duplicate live slot.
            let violations = d.checker.check().expect("integrity check");
            if !violations.is_empty() {
                self.fail(&format!(
                    "INVARIANT FAILURE on {}: IntegrityChecker.check() found:\n  {}",
                    d.name,
                    join_lines(&violations)
                ));
            }
            // 2. Every live clip's asset ROW exists (bytes may be absent in
            //    P0).
            let orphan_clips: Vec<String> =
                d.db.query(
                    "SELECT c.id AS id FROM clips c WHERE c.deleted_at IS NULL AND NOT EXISTS \
                     (SELECT 1 FROM assets a WHERE a.id = c.asset_id)",
                    [],
                    |r| r.get(0),
                )
                .expect("query");
            if !orphan_clips.is_empty() {
                self.fail(&format!(
                    "INVARIANT FAILURE on {}: live clips without asset rows: {}",
                    d.name,
                    orphan_clips.join(", ")
                ));
            }
            // 3. Oplog fully drained.
            let oplog = oplog_len(&d.db);
            if oplog != 0 {
                self.fail(&format!(
                    "INVARIANT FAILURE on {}: oplog not drained ({oplog} rows)",
                    d.name
                ));
            }
            // 4. Nothing quarantined: the episode injects no HLC drift and no
            //    malformed rows, so any quarantine row is an unexplained
            //    rejection.
            let parked = d.quarantine.list().expect("quarantine");
            if !parked.is_empty() {
                self.fail(&format!(
                    "INVARIANT FAILURE on {}: unexplained quarantine rows:\n  {}",
                    d.name,
                    parked
                        .iter()
                        .map(|q| format!("{}/{}: {}", q.tbl, q.id, q.reason))
                        .collect::<Vec<_>>()
                        .join("\n  ")
                ));
            }
            // 5. The change token sits exactly at the cloud head.
            let token = d.state.change_token().expect("change token");
            if token.as_deref() != Some(head.as_str()) {
                self.fail(&format!(
                    "INVARIANT FAILURE on {}: change token {} != cloud head {head}",
                    d.name,
                    token.as_deref().unwrap_or("null")
                ));
            }
        }
    }

    fn assert_oracle(&self) {
        // Roots: the recorded write with the max HLC decides liveness
        // everywhere. In particular an explicitly deleted root whose delete
        // out-HLCs every editor save must be tombstoned on ALL devices —
        // tombstones never resurrect; a root revived by a higher-HLC editor
        // save (rule 3 at the LWW level: the active editor wins) must be live
        // on ALL devices.
        for handle in self.practice_handles.iter().chain(&self.technique_handles) {
            let table = self.oracle.root_table[handle];
            let id = &self.oracle.root_id[handle];
            let expect_live = self.oracle.expected_live(handle);
            for d in &self.devices {
                let Some(row) = deleted_at(&d.db, table, id) else {
                    self.fail(&format!(
                        "ORACLE FAILURE: {handle} ({table}/{id}) has NO row on {} at \
                         quiescence (row lost)",
                        d.name
                    ));
                };
                let live = row.is_none();
                if live != expect_live {
                    let writes = self.oracle.root_writes[handle]
                        .iter()
                        .map(|(h, l)| format!("{h}={}", if *l { "live" } else { "delete" }))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let states: Vec<String> = self
                        .devices
                        .iter()
                        .map(|dev| {
                            let r = dev
                                .db
                                .query_opt(
                                    &format!(
                                        "SELECT hlc_packed AS h, deleted_at AS x FROM {table} \
                                         WHERE id = ?"
                                    ),
                                    [id],
                                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?)),
                                )
                                .expect("query");
                            format!(
                                "{}: {}",
                                dev.name,
                                match r {
                                    None => "<missing>".to_string(),
                                    Some((h, x)) => format!("{h}|{}", dart_opt(x)),
                                }
                            )
                        })
                        .collect();
                    // The cloud log keeps every accepted version in push
                    // order — the forensic trail of who uploaded what.
                    let history: Vec<String> = self
                        .cloud
                        .log()
                        .iter()
                        .filter(|entry| {
                            entry.row.table() == table && entry.row.id().ok() == Some(id.as_str())
                        })
                        .map(|entry| {
                            format!(
                                "seq{}: {}|{}",
                                entry.seq,
                                entry.row.hlc_packed().unwrap_or("?"),
                                entry
                                    .row
                                    .field("deleted_at")
                                    .map_or("null".to_string(), |v| v.dart_to_string())
                            )
                        })
                        .collect();
                    self.fail(&format!(
                        "ORACLE FAILURE: {handle} ({table}/{id}) on {} is {} but the recorded \
                         writes say {} (deleted: {})\n  recorded writes: {writes}\n  row state: \
                         {}\n  cloud history:\n    {}",
                        d.name,
                        if live { "LIVE" } else { "TOMBSTONED" },
                        if expect_live { "live" } else { "tombstoned" },
                        self.oracle.ever_deleted.contains(handle),
                        states.join("; "),
                        history.join("\n    "),
                    ));
                }
                // Content canary: a live root must show the content of the
                // highest-HLC OBSERVED committed state. Derived writes
                // (heals, slot resolutions) raise a row's HLC but never its
                // content, so any mismatch means a full-row snapshot reverted
                // a real edit — the derived-repair-stamp lost-update class
                // (docs/sync-protocol.md, "Known design gap").
                if expect_live {
                    let col = if table == "practices" {
                        "title"
                    } else {
                        "name"
                    };
                    let expected = self.oracle.expected_content(handle);
                    let actual: Option<String> =
                        d.db.query_row(
                            &format!("SELECT {col} AS c FROM {table} WHERE id = ?"),
                            [id],
                            |r| r.get(0),
                        )
                        .expect("query");
                    if let Some(expected) = expected {
                        if actual.as_deref() != Some(expected) {
                            self.fail(&format!(
                                "ORACLE FAILURE: {handle} ({table}/{id}) on {} shows {col} \
                                 \"{}\" but the highest-HLC observed state holds \"{expected}\" \
                                 — a full-row snapshot reverted a real edit \
                                 (derived-repair-stamp lost update)",
                                d.name,
                                actual.as_deref().unwrap_or("null"),
                            ));
                        }
                    }
                }
            }
        }
        // Children: an explicitly removed child may be live at quiescence
        // ONLY if some editor content-edited or inserted it at some point (the
        // active-editor-wins revive); everything else must stay tombstoned
        // everywhere.
        for (table, id) in &self.oracle.explicit_child_deletes {
            if self
                .oracle
                .live_touched
                .contains(&(table.clone(), id.clone()))
            {
                continue;
            }
            for d in &self.devices {
                if deleted_at(&d.db, table, id) == Some(None) {
                    self.fail(&format!(
                        "ORACLE FAILURE: explicitly removed {table}/{id} resurrected on {} \
                         although no editor ever touched it",
                        d.name
                    ));
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// `deleted_at` of row [id] of [table]: None when the row does not exist,
/// Some(None) when it is live.
fn deleted_at(db: &AppDatabase, table: &str, id: &str) -> Option<Option<i64>> {
    db.query_opt(
        &format!("SELECT deleted_at AS x FROM {table} WHERE id = ?"),
        [id],
        |r| r.get::<_, Option<i64>>(0),
    )
    .expect("query")
}

fn is_live(db: &AppDatabase, table: &str, id: &str) -> bool {
    deleted_at(db, table, id) == Some(None)
}

fn oplog_len(db: &AppDatabase) -> i64 {
    db.query_row("SELECT COUNT(*) FROM oplog", [], |r| r.get(0))
        .expect("query")
}

/// A Dart `int?` interpolated: the number, or `null`.
fn dart_opt(v: Option<i64>) -> String {
    v.map_or("null".to_string(), |v| v.to_string())
}

fn join_lines<T: Display>(items: &[T]) -> String {
    items
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n  ")
}

fn describe_push(r: &PushRun) -> String {
    match &r.failure {
        Some(f) => format!("FAILED({})", first_line(f)),
        None => format!("pushed:{} held:{}", r.pushed, r.held_back),
    }
}

fn describe_pull(r: &PullRun) -> String {
    match &r.failure {
        Some(f) => format!(
            "pages:{} applied:{} FAILED({})",
            r.pages,
            r.applied,
            first_line(f)
        ),
        None => format!(
            "pages:{} applied:{} skipped:{}",
            r.pages, r.applied, r.skipped
        ),
    }
}

/// The first line of [o]'s text, cut at 90 characters.
fn first_line(o: &dyn Display) -> String {
    let s = o.to_string();
    let line = s.split('\n').next().unwrap_or_default();
    if line.chars().count() > 90 {
        format!("{}…", line.chars().take(90).collect::<String>())
    } else {
        line.to_string()
    }
}

/// Full-DB snapshot of every synced table: 'table/id' ->
/// 'hlcPacked|deletedAt|contentHash'. Content is hashed through the SAME
/// wave-1 sync_hashes projections both save and hydrate use, for LIVE rows;
/// tombstones compare (hlc, deletedAt) only. Assets compare their durable
/// content identity (hash/size/ext/kind) — local_rel_path and the P2 byte
/// markers are per-device by design.
fn snapshot(db: &AppDatabase) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut put =
        |table: &str, id: &str, hlc: &str, del: Option<i64>, hash: &dyn Fn() -> String| {
            out.insert(
                format!("{table}/{id}"),
                format!(
                    "{hlc}|{}|{}",
                    dart_opt(del),
                    if del.is_none() {
                        hash()
                    } else {
                        "-".to_string()
                    }
                ),
            );
        };
    for r in db.select_all::<PracticeRow>().expect("practices") {
        put("practices", &r.id, &r.hlc_packed, r.deleted_at, &|| {
            practice_row_hash(&r).expect("hash")
        });
    }
    for r in db
        .select_all::<InterpretationRow>()
        .expect("interpretations")
    {
        put(
            "interpretations",
            &r.id,
            &r.hlc_packed,
            r.deleted_at,
            &|| interpretation_row_hash(&r),
        );
    }
    for r in db.select_all::<TrackRow>().expect("tracks") {
        put("tracks", &r.id, &r.hlc_packed, r.deleted_at, &|| {
            track_row_hash(&r).expect("hash")
        });
    }
    for r in db.select_all::<ClipRow>().expect("clips") {
        put("clips", &r.id, &r.hlc_packed, r.deleted_at, &|| {
            clip_row_hash(&r)
        });
    }
    for r in db.select_all::<PracticeNoteRow>().expect("practice_notes") {
        put(
            "practice_notes",
            &r.id,
            &r.hlc_packed,
            r.deleted_at,
            &|| practice_note_row_hash(&r),
        );
    }
    for r in db
        .select_all::<TechniqueMarkerRow>()
        .expect("technique_markers")
    {
        put(
            "technique_markers",
            &r.id,
            &r.hlc_packed,
            r.deleted_at,
            &|| technique_marker_row_hash(&r),
        );
    }
    for r in db.select_all::<FxBusRow>().expect("fx_buses") {
        put("fx_buses", &r.id, &r.hlc_packed, r.deleted_at, &|| {
            fx_bus_row_hash(&r).expect("hash")
        });
    }
    for r in db.select_all::<TechniqueRow>().expect("techniques") {
        put("techniques", &r.id, &r.hlc_packed, r.deleted_at, &|| {
            technique_row_hash(&r)
        });
    }
    for r in db
        .select_all::<TechniqueMediaRow>()
        .expect("technique_media")
    {
        put(
            "technique_media",
            &r.id,
            &r.hlc_packed,
            r.deleted_at,
            &|| technique_media_row_hash(&r),
        );
    }
    for r in db.select_all::<AssetRow>().expect("assets") {
        put("assets", &r.id, &r.hlc_packed, r.deleted_at, &|| {
            format!("{}:{}:{}:{}", r.content_hash, r.byte_size, r.ext, r.kind)
        });
    }
    out
}
