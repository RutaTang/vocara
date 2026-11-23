//! Ports of the exporter parts of `client/test/export_take_test.dart` and of
//! the example-export cases of `client/test/technique_detail_layout_test.dart`.
//!
//! The Dart tests drove the exporter through the song and technique view
//! models; their file names are the ones those view models suggest
//! (`AudioExporter.fileName` over the practice or technique and the item),
//! and their outcomes the messages the exporter returns for the toast. What
//! only the view models decide (a multi-clip take refused before the panel,
//! clips numbered by their place on the timeline) is left to their ports.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use vocara_model::util::export_copy::export_copy;
use vocara_services::audio_exporter::{AudioExporter, PickError};
use vocara_services::audio_file_service::AudioFileService;
use vocara_services::local_storage_service::LocalStorageService;
use vocara_services::recording_finalizer::RecordingFinalizer;

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// One test's files: the take's audio in a temp folder, and the app's own
/// storage rooted at `app/` beside it (the Dart tests' rooted storage).
struct Fixture {
    tmp: tempfile::TempDir,
    take: String,
    take_bytes: Vec<u8>,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let take_bytes: Vec<u8> = (0..96).collect();
        let take = tmp.path().join("take1.wav");
        fs::write(&take, &take_bytes).unwrap();
        Fixture {
            take: s(&take),
            tmp,
            take_bytes,
        }
    }

    fn path(&self, name: &str) -> String {
        s(&self.tmp.path().join(name))
    }

    /// Audio files whose app storage is rooted at [root].
    fn files(root: &Path) -> Arc<AudioFileService> {
        Arc::new(AudioFileService::new(Arc::new(
            LocalStorageService::with_root(root),
        )))
    }

    /// An exporter whose save panel records each suggested name and answers
    /// [pick]; its quit held on a finalizer of its own.
    fn exporter(
        &self,
        picks: &Arc<Mutex<Vec<String>>>,
        pick: impl Fn(&str) -> Result<Option<String>, PickError> + Send + Sync + 'static,
    ) -> AudioExporter {
        self.exporter_rooted(&self.tmp.path().join("app"), picks, pick)
    }

    fn exporter_rooted(
        &self,
        root: &Path,
        picks: &Arc<Mutex<Vec<String>>>,
        pick: impl Fn(&str) -> Result<Option<String>, PickError> + Send + Sync + 'static,
    ) -> AudioExporter {
        let picks = Arc::clone(picks);
        AudioExporter::new(Self::files(root), move |name| {
            picks.lock().unwrap().push(name.to_string());
            pick(name)
        })
        .with_finalizer(Arc::new(RecordingFinalizer::for_testing()))
    }

    /// `AudioExporter.fileName('My Song', 'Take 1', take)`, as the song view
    /// model suggests it for a single-clip take.
    fn take_name(&self) -> String {
        AudioExporter::file_name("My Song", "Take 1", &self.take, "")
    }

    /// The song screen's export of the take (noun "take", medium "audio").
    fn export_take(&self, exporter: &AudioExporter) -> Option<String> {
        exporter.export(&self.take, &self.take_name(), "take", "audio")
    }
}

fn picks() -> Arc<Mutex<Vec<String>>> {
    Arc::default()
}

// A quit mid-copy exited under the export and left the file at the picked
// path cut short (an old file there already replaced).
#[test]
fn an_export_holds_the_quit_while_it_copies() {
    let f = Fixture::new();
    let dest = f.path("exported.wav");
    let finalizer = Arc::new(RecordingFinalizer::for_testing());
    let held = Arc::new(AtomicBool::new(false));
    let exporter = {
        let (to, finalizer, held) = (dest.clone(), Arc::clone(&finalizer), Arc::clone(&held));
        let copy_finalizer = Arc::clone(&finalizer);
        AudioExporter::new(Fixture::files(&f.tmp.path().join("app")), move |_| {
            Ok(Some(to.clone()))
        })
        .with_finalizer(finalizer)
        .with_copy(move |source, dest| {
            held.store(copy_finalizer.has_active(), Ordering::SeqCst);
            export_copy(source, dest)
        })
    };
    assert!(!finalizer.has_active());

    let said = f.export_take(&exporter);

    assert!(
        held.load(Ordering::SeqCst),
        "the copy ran with the quit held"
    );
    assert!(!finalizer.has_active(), "and released after");
    assert_eq!(said, Some(format!("Exported to {dest}")));
    assert!(Path::new(&dest).exists());
}

#[test]
fn exports_a_single_clip_take_byte_for_byte_to_the_picked_path() {
    let f = Fixture::new();
    let dest = f.path("exported.wav");
    let picks = picks();
    let to = dest.clone();
    let exporter = f.exporter(&picks, move |_| Ok(Some(to.clone())));

    let said = f.export_take(&exporter);

    assert_eq!(*picks.lock().unwrap(), ["My Song - Take 1.wav"]);
    assert_eq!(fs::read(&dest).unwrap(), f.take_bytes);
    assert_eq!(said, Some(format!("Exported to {dest}")));
}

#[test]
fn an_export_onto_the_takes_own_file_is_refused_its_audio_kept() {
    // The save panel can browse into the app's storage: copying a file onto
    // itself truncated it to nothing, and the toast said "Exported".
    let f = Fixture::new();
    let own = f.take.clone();
    let exporter = f.exporter(&picks(), move |_| Ok(Some(own.clone())));

    let said = f.export_take(&exporter).unwrap();

    assert_eq!(fs::read(&f.take).unwrap(), f.take_bytes);
    assert!(said.contains("own audio file"), "{said}");
}

#[test]
fn and_so_is_one_onto_a_link_to_it() {
    let f = Fixture::new();
    let link = f.path("linked.wav");
    symlink(&f.take, &link).unwrap();
    let exporter = f.exporter(&picks(), move |_| Ok(Some(link.clone())));

    let said = f.export_take(&exporter).unwrap();

    assert_eq!(fs::read(&f.take).unwrap(), f.take_bytes);
    assert!(said.contains("own audio file"), "{said}");
}

#[test]
fn and_so_is_one_onto_a_hard_link_to_it() {
    // (sameFile: "a symlink or hard link to it too".)
    let f = Fixture::new();
    let link = f.path("hard.wav");
    fs::hard_link(&f.take, &link).unwrap();
    assert!(AudioExporter::same_file(&f.take, &link));
    assert!(!AudioExporter::same_file(&f.take, &f.path("nothing.wav")));
}

#[test]
fn an_export_into_the_apps_own_storage_is_refused() {
    // The take pool and the library database live there: an export replacing
    // one overwrote another take's audio, or the library itself.
    let f = Fixture::new();
    let library = f.tmp.path().join("library");
    fs::create_dir(&library).unwrap();
    let db = library.join("vocara.sqlite");
    fs::write(&db, [1, 2, 3]).unwrap();
    let to = s(&db);
    let exporter = f.exporter_rooted(&library, &picks(), move |_| Ok(Some(to.clone())));

    let said = f.export_take(&exporter).unwrap();

    assert_eq!(fs::read(&db).unwrap(), [1, 2, 3]);
    assert!(said.contains("Vocara's own files"), "{said}");
}

#[test]
fn and_so_is_a_link_that_names_a_new_file_there() {
    let f = Fixture::new();
    let library = f.tmp.path().join("library");
    let target = library.join("audio").join("p1").join("x.wav");
    let link = f.path("outside.wav");
    symlink(&target, &link).unwrap();
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    let exporter = f.exporter_rooted(&library, &picks(), move |_| Ok(Some(link.clone())));

    let said = f.export_take(&exporter).unwrap();

    assert!(!target.exists());
    assert!(said.contains("Vocara's own files"), "{said}");
}

#[test]
fn a_cancelled_save_panel_exports_nothing_and_stays_silent() {
    let f = Fixture::new();
    let picks = picks();
    let exporter = f.exporter(&picks, |_| Ok(None));

    let said = f.export_take(&exporter);

    assert_eq!(picks.lock().unwrap().len(), 1); // the panel opened…
    assert_eq!(said, None); // …but a cancel is not an outcome to report
    assert!(!exporter.exporting());
}

#[test]
fn a_save_panel_that_fails_to_open_says_so() {
    let f = Fixture::new();
    let failing = f.exporter(&picks(), |_| Err("panel failed".into()));
    assert_eq!(
        f.export_take(&failing).as_deref(),
        Some("Could not open the save dialog.")
    );
    let panicking = f.exporter(&picks(), |_| panic!("panel failed"));
    assert_eq!(
        f.export_take(&panicking).as_deref(),
        Some("Could not open the save dialog.")
    );
    assert!(!panicking.exporting(), "a failed panel releases the export");
}

#[test]
fn a_clip_of_a_multi_clip_take_is_suggested_with_its_number() {
    let f = Fixture::new();
    let clip = f.path("clip_b.wav");
    fs::write(&clip, [2u8; 64]).unwrap();
    let dest = f.path("clip_export.wav");
    let picks = picks();
    let to = dest.clone();
    let exporter = f.exporter(&picks, move |_| Ok(Some(to.clone())));
    let name = AudioExporter::file_name("My Song", "Take 2", &clip, " (clip 2)");

    exporter.export(&clip, &name, "clip", "audio");

    assert_eq!(*picks.lock().unwrap(), ["My Song - Take 2 (clip 2).wav"]);
    assert_eq!(fs::read(&dest).unwrap(), [2u8; 64]);
}

#[test]
fn a_second_export_while_one_runs_is_refused() {
    // Nothing showed a copy running, and two copies to one chosen name wrote
    // one file together, each then saying "Exported".
    let f = Fixture::new();
    let (opened_tx, opened) = mpsc::channel();
    let (answer, panel) = mpsc::channel::<Option<String>>();
    let opened_tx = Mutex::new(opened_tx);
    let panel = Mutex::new(panel);
    let exporter = Arc::new(f.exporter(&picks(), move |_| {
        opened_tx.lock().unwrap().send(()).unwrap();
        Ok(panel.lock().unwrap().recv().unwrap())
    }));
    let first = {
        let (exporter, take, name) = (Arc::clone(&exporter), f.take.clone(), f.take_name());
        thread::spawn(move || exporter.export(&take, &name, "take", "audio"))
    };
    opened.recv().unwrap(); // the first export's panel is open
    assert!(exporter.exporting());

    assert_eq!(
        f.export_take(&exporter).as_deref(),
        Some("Wait for the current export to finish.")
    );

    answer.send(None).unwrap();
    assert_eq!(first.join().unwrap(), None);
    assert!(!exporter.exporting());
}

#[test]
fn a_source_gone_while_the_save_panel_was_open_is_said_so() {
    // The copy then failed as if the destination had.
    let f = Fixture::new();
    let dest = f.path("late.wav");
    let (source, to) = (f.take.clone(), dest.clone());
    let exporter = f.exporter(&picks(), move |_| {
        fs::remove_file(&source).unwrap();
        Ok(Some(to.clone()))
    });

    let said = f.export_take(&exporter).unwrap();

    assert!(said.contains("missing"), "{said}");
    assert!(!Path::new(&dest).exists());
}

#[test]
fn a_copy_that_fails_is_blamed_on_the_destination_while_the_source_is_there() {
    let f = Fixture::new();
    let dest = f.path("no-such-folder/out.wav");
    let to = dest.clone();
    let exporter = f.exporter(&picks(), move |_| Ok(Some(to.clone())));
    assert_eq!(
        f.export_take(&exporter).as_deref(),
        Some("Could not export the audio to that location.")
    );
}

#[test]
fn a_long_title_is_cut_to_a_name_the_file_system_takes() {
    // A file name holds 255 bytes; an unbounded title suggested one the copy
    // then failed on, with only "Could not export" to say why.
    let name = AudioExporter::file_name(&"歌".repeat(120), "Take 1", "take1.wav", "");
    assert!(name.len() <= 255, "{}", name.len());
    assert!(name.ends_with("歌 - Take 1.wav"), "{name}");
}

#[test]
fn a_long_take_name_keeps_its_clip_number() {
    // The number was cut with the name: two clips suggested one file name.
    let long = "a full set recorded live at the rehearsal room ".repeat(3);
    let first = AudioExporter::file_name("My Song", &long, "a.mp3", " (clip 1)");
    let second = AudioExporter::file_name("My Song", &long, "b.wav", " (clip 2)");
    assert!(first.ends_with(" (clip 1).mp3"), "{first}");
    assert!(second.ends_with(" (clip 2).wav"), "{second}");
}

#[test]
fn path_hostile_characters_are_flattened_and_a_bare_source_suggests_wav() {
    assert_eq!(
        AudioExporter::file_name("AC/DC: Live", "Take\\1", "/x/take", ""),
        "AC-DC- Live - Take-1.wav"
    );
}

#[test]
fn keeps_the_source_extension_for_a_backing_export() {
    let f = Fixture::new();
    let backing = f.path("backing.mp3");
    fs::write(&backing, [1u8; 64]).unwrap();
    let dest = f.path("backing_export.mp3");
    let picks = picks();
    let to = dest.clone();
    let exporter = f.exporter(&picks, move |_| Ok(Some(to.clone())));
    let name = AudioExporter::file_name("My Song", "Backing", &backing, "");

    exporter.export(&backing, &name, "track", "audio");

    assert_eq!(*picks.lock().unwrap(), ["My Song - Backing.mp3"]);
    assert!(Path::new(&dest).exists());
}

#[test]
fn a_missing_source_file_says_so_instead_of_opening_the_panel() {
    let f = Fixture::new();
    let picks = picks();
    let to = f.path("y.wav");
    let exporter = f.exporter(&picks, move |_| Ok(Some(to.clone())));
    fs::remove_file(&f.take).unwrap();

    let said = f.export_take(&exporter);

    assert!(picks.lock().unwrap().is_empty());
    assert_eq!(
        said.as_deref(),
        Some("The audio file for this take is missing.")
    );
    assert!(!exporter.exporting());
}

#[test]
fn bytes_not_on_this_device_yet_are_said_so_not_missing() {
    let f = Fixture::new();
    let picks = picks();
    let exporter = f.exporter(&picks, |_| Ok(None));
    assert_eq!(
        exporter
            .export(
                "asset://A0000000000000000000000000",
                "x.wav",
                "example",
                "video"
            )
            .as_deref(),
        Some("This example's video isn't on this device yet, so it can't be exported.")
    );
    assert!(picks.lock().unwrap().is_empty());
}

// From technique_detail_layout_test.dart: a recorded example was reachable
// only by digging through the app's container, its file named by a hash.
#[test]
fn an_example_can_be_exported() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("take.wav");
    fs::write(&source, [1, 2, 3, 4]).unwrap();
    let dest = tmp.path().join("out").join("exported.wav");
    fs::create_dir(dest.parent().unwrap()).unwrap();
    let suggested = picks();
    let exporter = {
        let (suggested, to) = (Arc::clone(&suggested), s(&dest));
        AudioExporter::new(Fixture::files(&tmp.path().join("app")), move |name| {
            suggested.lock().unwrap().push(name.to_string());
            Ok(Some(to.clone()))
        })
        .with_finalizer(Arc::new(RecordingFinalizer::for_testing()))
    };
    let source = s(&source);

    let said = exporter.export(
        &source,
        &AudioExporter::file_name("Twang", "Recording 1", &source, ""),
        "example",
        "audio",
    );

    assert_eq!(*suggested.lock().unwrap(), ["Twang - Recording 1.wav"]);
    assert_eq!(fs::read(&dest).unwrap(), [1, 2, 3, 4]);
    assert_eq!(said, Some(format!("Exported to {}", s(&dest))));
}

#[test]
fn a_video_examples_export_says_video_not_audio() {
    let tmp = tempfile::tempdir().unwrap();
    let exporter = AudioExporter::new(Fixture::files(tmp.path()), |_| {
        panic!("a missing file opens no panel")
    })
    .with_finalizer(Arc::new(RecordingFinalizer::for_testing()));
    let path = "/nonexistent/vocara/clip.mp4";

    let said = exporter.export(
        path,
        &AudioExporter::file_name("Twang", "clip", path, ""),
        "example",
        "video",
    );

    assert_eq!(
        said.as_deref(),
        Some("The video file for this example is missing.")
    );
}

#[test]
fn the_production_exporter_holds_the_app_wide_quit() {
    // Dart's default finalizer: RecordingFinalizer.instance.
    let f = Fixture::new();
    let dest = f.path("exported.wav");
    let held = Arc::new(AtomicBool::new(false));
    let exporter = {
        let (to, held) = (dest.clone(), Arc::clone(&held));
        AudioExporter::new(Fixture::files(&f.tmp.path().join("app")), move |_| {
            Ok(Some(to.clone()))
        })
        .with_copy(move |source, dest| {
            held.store(
                RecordingFinalizer::instance().has_active(),
                Ordering::SeqCst,
            );
            export_copy(source, dest)
        })
    };
    let said = f.export_take(&exporter);
    assert!(held.load(Ordering::SeqCst));
    assert_eq!(said, Some(format!("Exported to {dest}")));
}
