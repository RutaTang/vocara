//! The view model's cases of `song_practice_save_base_test.dart`, plus
//! `song_practice_dispose_save_test.dart` and
//! `song_practice_quit_save_test.dart`, end to end on the real store (an
//! in-memory database).
//!
//! The base a queued save merges against: when a second edit lands while a
//! save is in flight, the view model keeps its newer model rather than
//! adopting the re-read aggregate, and the queued save merges against what
//! the first one SENT — never the re-read's fresh base, which knows about
//! rows a concurrent writer added that the newer model has never seen.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use vocara_model::models::audio_track::{AudioTrack, AudioTrackKind};
use vocara_model::models::practice_note::PracticeNote;
use vocara_model::models::song_practice::SongPractice;
use vocara_model::util::id::new_id;
use vocara_store::db::{AppDatabase, AssetStore};
use vocara_store::repositories::SongPracticeRepositoryDrift;
use vocara_store::sync::hlc::HlcClock;
use vocara_store::sync::sync_writer::SyncWriter;

use super::*;

/// One test's library: the store, and a screen's collaborators over it.
struct Library {
    _tmp: tempfile::TempDir,
    db: Arc<AppDatabase>,
    root: std::path::PathBuf,
    repo: Arc<SongPracticeRepositoryDrift>,
}

impl Library {
    fn new() -> Library {
        let tmp = tempfile::tempdir().expect("a temp dir");
        let db = Arc::new(AppDatabase::open_in_memory().expect("a database"));
        let root = tmp.path().to_path_buf();
        let repo = Arc::new(Library::songs(&db, &root));
        Library {
            _tmp: tmp,
            db,
            root,
            repo,
        }
    }

    fn songs(db: &Arc<AppDatabase>, root: &std::path::Path) -> SongPracticeRepositoryDrift {
        let writer = Arc::new(SyncWriter::new(
            Arc::clone(db),
            Arc::new(HlcClock::new(format!("{:0<26}", "NODE"))),
        ));
        let assets = Arc::new(AssetStore::new(
            Arc::clone(db),
            root.to_path_buf(),
            Arc::clone(&writer),
        ));
        SongPracticeRepositoryDrift::new(Arc::clone(db), writer, assets)
    }

    /// Another repository over the same store whose next read can be made
    /// to fail (Dart's `_FlakyReread`): a save's re-read after its commit.
    fn flaky(&self) -> (Arc<SongPracticeRepositoryDrift>, Arc<AtomicBool>) {
        let repo = Arc::new(Library::songs(&self.db, &self.root));
        let fail = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&fail);
        repo.set_debug_load_hook(Some(Arc::new(move |_: &str| {
            if flag.swap(false, Ordering::SeqCst) {
                return Err(vocara_store::Error::State("re-read failed".into()));
            }
            Ok(())
        })));
        (repo, fail)
    }

    fn load(&self, id: &str) -> Loaded<SongPractice> {
        self.repo.load_practice(id).expect("a practice")
    }

    /// A practice with one take carrying `notes`, persisted.
    fn seed(&self, notes: Vec<PracticeNote>) -> SongPractice {
        let created = self.repo.create_practice("Song").expect("created");
        let mut take = AudioTrack::new(new_id(), AudioTrackKind::Vocal, "Take 1", at());
        take.version_id = created.active_version_id.clone();
        take.notes = notes;
        self.repo
            .save_practice(&SongPractice {
                vocal_tracks: vec![Arc::new(take)],
                ..created.clone()
            })
            .expect("saved");
        self.load(&created.id).aggregate
    }

    /// Another writer (the sync applier, another editor) changes the take's
    /// notes while the screen holds the aggregate.
    fn concurrently(&self, id: &str, edit: impl Fn(&[PracticeNote]) -> Vec<PracticeNote>) {
        let other = self.load(id);
        let take = Arc::clone(&other.aggregate.vocal_tracks[0]);
        let next = SongPractice {
            vocal_tracks: vec![Arc::new(AudioTrack {
                notes: edit(&take.notes),
                ..(*take).clone()
            })],
            ..other.aggregate.clone()
        };
        self.repo
            .save_practice_with_base(&next, &other.base_hashes)
            .expect("another writer saved");
    }

    /// Mutes the take behind the screen's back.
    fn mute_elsewhere(&self, id: &str) {
        let other = self.load(id);
        let take = Arc::clone(&other.aggregate.vocal_tracks[0]);
        let next = SongPractice {
            vocal_tracks: vec![Arc::new(AudioTrack {
                muted: true,
                ..(*take).clone()
            })],
            ..other.aggregate.clone()
        };
        self.repo
            .save_practice_with_base(&next, &other.base_hashes)
            .expect("another writer saved");
    }

    fn open(
        &self,
        id: &str,
        repo: Option<Arc<dyn SongPracticeRepository>>,
        finalizer: &Arc<RecordingFinalizer>,
    ) -> (Arc<FakeEngine>, Screen) {
        let engine = Arc::new(FakeEngine::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let deps = Deps {
            repository: repo
                .unwrap_or_else(|| Arc::clone(&self.repo) as Arc<dyn SongPracticeRepository>),
            technique_repository: Arc::new(FakeTechRepo::default()),
            audio_files: FakeFiles::new(events),
            spectrograms: Arc::new(NoSpectrograms),
            separation: Arc::new(FakeSourceSeparationService { available: false }),
            app_settings: settings(),
        };
        let options = Options {
            engine: Some(EngineHandle::Other(
                Arc::clone(&engine) as Arc<dyn PlaybackEngine>
            )),
            edits: Some(Arc::new(FakeEdits::default())),
            finalizer: Some(Arc::clone(finalizer)),
            probe_duration: Some(Arc::new(|_: &str| Ok(None))),
            analyze: Some(ok_analyze()),
            ..Options::default()
        };
        let mut screen = Screen::open(deps, id, options);
        screen.settle();
        assert!(screen.vm.practice().is_some());
        (engine, screen)
    }
}

fn note(content: &str) -> PracticeNote {
    PracticeNote {
        id: new_id(),
        position_ms: 10,
        end_ms: None,
        content: content.into(),
        created_at: at(),
        updated_at: at(),
    }
}

/// Two quick local edits: the second lands while the first save is still in
/// flight, so the model moves on under it.
fn rename_twice(s: &mut Screen, take_id: &str) {
    s.act(|vm| vm.rename_track(take_id, "A"));
    s.act(|vm| vm.rename_track(take_id, "B"));
    s.settle();
}

fn finalizer() -> Arc<RecordingFinalizer> {
    Arc::new(RecordingFinalizer::for_testing())
}

#[test]
fn a_concurrent_insert_survives_the_queued_save() {
    let lib = Library::new();
    let p = lib.seed(Vec::new());
    let f = finalizer();
    let (_e, mut s) = lib.open(&p.id, None, &f);
    let remote = note("from the other device");
    let remote_id = remote.id.clone();
    lib.concurrently(&p.id, |notes| {
        let mut n = notes.to_vec();
        n.push(remote.clone());
        n
    });
    rename_twice(&mut s, &p.vocal_tracks[0].id);
    let take = Arc::clone(&lib.load(&p.id).aggregate.vocal_tracks[0]);
    assert_eq!(take.display_name, "B");
    let ids: Vec<&str> = take.notes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(ids, [remote_id.as_str()]);
}

#[test]
fn a_concurrent_edit_survives_the_queued_save() {
    let lib = Library::new();
    let p = lib.seed(vec![note("as loaded")]);
    let f = finalizer();
    let (_e, mut s) = lib.open(&p.id, None, &f);
    lib.concurrently(&p.id, |notes| {
        vec![PracticeNote {
            content: "edited elsewhere".into(),
            ..notes[0].clone()
        }]
    });
    rename_twice(&mut s, &p.vocal_tracks[0].id);
    let take = Arc::clone(&lib.load(&p.id).aggregate.vocal_tracks[0]);
    assert_eq!(take.display_name, "B");
    assert_eq!(take.notes[0].content, "edited elsewhere");
}

#[test]
fn a_concurrent_delete_stays_deleted_after_the_queued_save() {
    let lib = Library::new();
    let p = lib.seed(vec![note("to be deleted elsewhere")]);
    let f = finalizer();
    let (_e, mut s) = lib.open(&p.id, None, &f);
    lib.concurrently(&p.id, |_| Vec::new());
    rename_twice(&mut s, &p.vocal_tracks[0].id);
    let take = Arc::clone(&lib.load(&p.id).aggregate.vocal_tracks[0]);
    assert_eq!(take.display_name, "B");
    assert!(take.notes.is_empty());
}

#[test]
fn a_lyrics_save_that_folded_in_a_concurrent_change_resyncs_the_engine() {
    // The lyrics path skips the engine resync so tap-to-time never glitches
    // playback, and remote reloads are skipped while a save is in flight —
    // so a concurrent change the lyrics save merged showed in the UI while
    // the engine kept playing the old mix.
    let lib = Library::new();
    let p = lib.seed(Vec::new());
    let f = finalizer();
    let (engine, mut s) = lib.open(&p.id, None, &f);
    let quiet = engine.syncs();
    let call = s.call(|vm| vm.set_lyrics_text("first line"));
    assert!(s.landed(call));
    s.settle();
    assert_eq!(engine.syncs(), quiet, "a plain lyric save stays quiet");
    lib.mute_elsewhere(&p.id);
    let call = s.call(|vm| vm.set_lyrics_text("second line"));
    assert!(s.landed(call));
    s.settle();
    assert!(s.practice().vocal_tracks[0].muted);
    assert!(engine.syncs() > quiet, "the mix must follow");
}

#[test]
fn a_routing_edit_syncs_the_engine_once_through_its_save() {
    // The save's tail resyncs the engine with what it saved; the mixer's
    // routing edits used to run a second, identical sync after it.
    let lib = Library::new();
    let p = lib.seed(Vec::new());
    let f = finalizer();
    let (engine, mut s) = lib.open(&p.id, None, &f);
    let take = p.vocal_tracks[0].id.clone();

    let before = engine.syncs();
    s.act(|vm| vm.mixer_do(|m, h| m.create_bus(h, "Verb")));
    s.settle();
    assert_eq!(engine.syncs() - before, 1);
    let bus = s.practice().fx_buses[0].id.clone();

    let before = engine.syncs();
    s.act(|vm| vm.mixer_do(|m, h| m.set_track_bus(h, &take, Some(&bus))));
    s.settle();
    assert_eq!(engine.syncs() - before, 1);
    assert_eq!(
        s.practice().vocal_tracks[0].bus_id.as_deref(),
        Some(bus.as_str())
    );

    let before = engine.syncs();
    s.act(|vm| vm.mixer_do(|m, h| m.add_track_send(h, &take, &bus)));
    s.settle();
    assert_eq!(engine.syncs() - before, 1);

    let before = engine.syncs();
    s.act(|vm| vm.mixer_do(|m, h| m.delete_bus(h, &bus)));
    s.settle();
    assert_eq!(engine.syncs() - before, 1);
    assert!(s.practice().fx_buses.is_empty());
}

#[test]
fn a_save_whose_re_read_fails_says_nothing_failed() {
    let lib = Library::new();
    let p = lib.seed(Vec::new());
    let (flaky, fail) = lib.flaky();
    let f = finalizer();
    let (_e, mut s) = lib.open(&p.id, Some(flaky), &f);
    fail.store(true, Ordering::SeqCst);
    s.act(|vm| vm.rename_track(&p.vocal_tracks[0].id, "renamed"));
    s.settle();
    assert!(s.toasts.is_empty());
    assert_eq!(s.practice().vocal_tracks[0].display_name, "renamed");
}

#[test]
fn and_then_shows_what_another_writer_stored_meanwhile() {
    // The stand-in holds only what was sent: a note another writer edited,
    // kept by the merge, stayed off screen as loaded, and the next local
    // edit of it rolled the other writer's change back.
    let lib = Library::new();
    let p = lib.seed(vec![note("as loaded")]);
    let (flaky, fail) = lib.flaky();
    let f = finalizer();
    let (_e, mut s) = lib.open(&p.id, Some(flaky), &f);
    lib.concurrently(&p.id, |notes| {
        vec![PracticeNote {
            content: "edited elsewhere".into(),
            ..notes[0].clone()
        }]
    });
    fail.store(true, Ordering::SeqCst);
    s.act(|vm| vm.rename_track(&p.vocal_tracks[0].id, "renamed"));
    s.settle();
    let take = Arc::clone(&s.practice().vocal_tracks[0]);
    assert_eq!(take.display_name, "renamed");
    assert_eq!(take.notes[0].content, "edited elsewhere");
}

#[test]
fn after_a_lyrics_save_too_and_resyncs_the_mix_it_changed() {
    let lib = Library::new();
    let p = lib.seed(Vec::new());
    let (flaky, fail) = lib.flaky();
    let f = finalizer();
    let (engine, mut s) = lib.open(&p.id, Some(flaky), &f);
    lib.mute_elsewhere(&p.id);
    let syncs = engine.syncs();
    fail.store(true, Ordering::SeqCst);
    let call = s.call(|vm| vm.set_lyrics_text("first line"));
    assert!(s.landed(call));
    s.settle();
    assert!(s.practice().vocal_tracks[0].muted);
    assert!(engine.syncs() > syncs, "the mix must follow");
}

#[test]
fn but_never_over_an_edit_made_while_that_save_ran() {
    // The catch-up judged "moved on" after the save, against whatever was on
    // screen by then: an edit made during the save was read over with the
    // store, and its own queued save then wrote nothing and said it landed.
    let lib = Library::new();
    let p = lib.seed(Vec::new());
    let (flaky, fail) = lib.flaky();
    let f = finalizer();
    let (_e, mut s) = lib.open(&p.id, Some(flaky), &f);
    fail.store(true, Ordering::SeqCst);
    rename_twice(&mut s, &p.vocal_tracks[0].id);
    assert_eq!(s.practice().vocal_tracks[0].display_name, "B");
    assert_eq!(lib.load(&p.id).aggregate.vocal_tracks[0].display_name, "B");
}

#[test]
fn nor_over_lyrics_saved_again_while_it_ran() {
    let lib = Library::new();
    let p = lib.seed(Vec::new());
    let (flaky, fail) = lib.flaky();
    let f = finalizer();
    let (_e, mut s) = lib.open(&p.id, Some(flaky), &f);
    fail.store(true, Ordering::SeqCst);
    let first = s.call(|vm| vm.set_lyrics_text("one"));
    let second = s.call(|vm| vm.set_lyrics_text("two"));
    assert!(s.landed(first));
    assert!(s.landed(second));
    s.settle();
    let words = |p: &SongPractice| {
        p.lyrics
            .iter()
            .map(|l| l.text.clone())
            .collect::<Vec<_>>()
            .join("|")
    };
    assert_eq!(words(s.practice()), "two");
    assert_eq!(words(&lib.load(&p.id).aggregate), "two");
}

#[test]
fn and_a_plain_lyrics_stand_in_leaves_the_mix_alone() {
    // Resynced only when the store differs from what was sent: a lyric stamp
    // never glitches playback.
    let lib = Library::new();
    let p = lib.seed(Vec::new());
    let (flaky, fail) = lib.flaky();
    let f = finalizer();
    let (engine, mut s) = lib.open(&p.id, Some(flaky), &f);
    let syncs = engine.syncs();
    fail.store(true, Ordering::SeqCst);
    let call = s.call(|vm| vm.set_lyrics_text("first line"));
    assert!(s.landed(call));
    s.settle();
    assert_eq!(engine.syncs(), syncs);
}

// --- song_practice_dispose_save_test.dart -------------------------------------------

/// The store, with a gate the next save parks on (Dart's `_Parking`).
struct Parking {
    inner: Arc<SongPracticeRepositoryDrift>,
    gate: Mutex<Option<Arc<Gate>>>,
    parked: AtomicUsize,
}

impl SongPracticeRepository for Parking {
    fn list_practices(&self) -> vocara_store::Result<Vec<SongPractice>> {
        self.inner.list_practices()
    }

    fn list_practice_summaries(&self) -> vocara_store::Result<Vec<PracticeSummary>> {
        self.inner.list_practice_summaries()
    }

    fn get_practice(&self, id: &str) -> vocara_store::Result<SongPractice> {
        self.inner.get_practice(id)
    }

    fn load_practice(&self, id: &str) -> vocara_store::Result<Loaded<SongPractice>> {
        self.inner.load_practice(id)
    }

    fn create_practice(&self, title: &str) -> vocara_store::Result<SongPractice> {
        self.inner.create_practice(title)
    }

    fn save_practice(&self, practice: &SongPractice) -> vocara_store::Result<SongPractice> {
        self.inner.save_practice(practice)
    }

    fn save_practice_with_base(
        &self,
        practice: &SongPractice,
        base_hashes: &IndexMap<String, String>,
    ) -> vocara_store::Result<Loaded<SongPractice>> {
        let gate = guard(&self.gate).take();
        if let Some(gate) = gate {
            self.parked.fetch_add(1, Ordering::SeqCst);
            gate.wait();
        }
        self.inner.save_practice_with_base(practice, base_hashes)
    }

    fn delete_practice(&self, id: &str) -> vocara_store::Result<()> {
        self.inner.delete_practice(id)
    }
}

fn mute_then_unmute_during_one_save(leave: bool) {
    let lib = Library::new();
    let p = lib.seed(Vec::new());
    let take = p.vocal_tracks[0].id.clone();
    let parking = Arc::new(Parking {
        inner: Arc::clone(&lib.repo),
        gate: Mutex::new(None),
        parked: AtomicUsize::new(0),
    });
    let f = finalizer();
    let (_e, mut s) = lib.open(
        &p.id,
        Some(Arc::clone(&parking) as Arc<dyn SongPracticeRepository>),
        &f,
    );
    let release = Gate::new();
    *guard(&parking.gate) = Some(Arc::clone(&release));
    s.act(|vm| vm.mixer_do(|m, h| m.toggle_mute(h, &take))); // save A sends muted
    s.pump_until(|_| parking.parked.load(Ordering::SeqCst) == 1);
    s.act(|vm| vm.mixer_do(|m, h| m.toggle_mute(h, &take))); // queued, model unmuted
    assert!(!s.practice().vocal_tracks[0].muted);
    if leave {
        s.vm.dispose(); // Back: the screen goes while A is in flight
    }
    release.open();
    s.settle();
    f.settled(&p.id);
    // The user's last word was "unmuted".
    assert!(!lib.load(&p.id).aggregate.vocal_tracks[0].muted);
}

#[test]
fn mute_then_unmute_during_one_save_staying() {
    mute_then_unmute_during_one_save(false);
}

#[test]
fn mute_then_unmute_during_one_save_leaving() {
    mute_then_unmute_during_one_save(true);
}

// --- song_practice_quit_save_test.dart ----------------------------------------------

#[test]
fn a_quit_during_a_queued_practice_save_waits_for_it_to_land() {
    let lib = Library::new();
    let p = lib.seed(Vec::new());
    let take = p.vocal_tracks[0].id.clone();
    let parking = Arc::new(Parking {
        inner: Arc::clone(&lib.repo),
        gate: Mutex::new(None),
        parked: AtomicUsize::new(0),
    });
    let f = finalizer();
    let (_e, mut s) = lib.open(
        &p.id,
        Some(Arc::clone(&parking) as Arc<dyn SongPracticeRepository>),
        &f,
    );
    assert!(!f.has_active(), "nothing in flight yet");
    let release = Gate::new();
    *guard(&parking.gate) = Some(Arc::clone(&release));
    s.act(|vm| vm.rename_track(&take, "Keeper"));
    assert!(f.has_active(), "held from the edit on");

    let quit_done = Arc::new(AtomicBool::new(false));
    let quit = {
        let f = Arc::clone(&f);
        let done = Arc::clone(&quit_done);
        thread::spawn(move || {
            f.finalize_all();
            done.store(true, Ordering::SeqCst);
        })
    };
    thread::sleep(Duration::from_millis(20));
    assert!(
        !quit_done.load(Ordering::SeqCst),
        "the quit waits for the airborne save"
    );
    release.open();
    s.settle();
    quit.join().expect("the quit ends");
    assert_eq!(
        lib.repo.get_practice(&p.id).expect("stored").vocal_tracks[0].display_name,
        "Keeper"
    );
    assert!(!f.has_active());
}

// The lyrics editor's draft outlives the editor widget (a tab switch
// disposes it), but it is the user's to apply: neither another edit's save
// nor a quit may write it, and closing the editor drops it.
#[test]
fn an_unsaved_lyrics_draft_is_never_saved_behind_the_user() {
    let lib = Library::new();
    let p = lib.seed(Vec::new());
    let take = p.vocal_tracks[0].id.clone();
    let f = finalizer();
    let (_e, mut s) = lib.open(&p.id, None, &f);
    s.vm.set_lyrics_mode(LyricsMode::Edit);
    s.vm.set_lyrics_draft(Some("never saved".into()));
    s.act(|vm| vm.rename_track(&take, "Keeper"));
    s.settle();
    f.finalize_all();
    let stored = lib.repo.get_practice(&p.id).expect("stored");
    assert_eq!(stored.vocal_tracks[0].display_name, "Keeper");
    assert!(stored.lyrics.is_empty(), "only Save applies a draft");
    assert!(s.vm.lyrics().is_empty());
    s.vm.set_lyrics_mode(LyricsMode::Read);
    assert_eq!(s.vm.lyrics_draft(), None, "closing the editor drops it");
}
